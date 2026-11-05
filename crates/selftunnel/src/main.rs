use std::{
    fs,
    io::BufReader,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::mpsc as std_mpsc,
    thread,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use eframe::egui;
use selfprotocol::{read_auth_result, read_frame, send_auth, write_frame, Frame};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{lookup_host, TcpStream},
    sync::{mpsc, watch, Mutex},
    time::{interval, timeout},
};
use tokio_rustls::{
    rustls::{self, pki_types::ServerName, RootCertStore},
    TlsConnector,
};
use tracing::{info, warn};

const MAX_STREAM_BUFFER: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct AppConfig {
    server_address: String,
    access_token: String,
    local_address: String,
    ca_certificate: String,
    auto_connect: bool,
    auto_reconnect: bool,
    start_with_windows: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server_address: String::new(),
            access_token: String::new(),
            local_address: "127.0.0.1:25565".to_owned(),
            ca_certificate: String::new(),
            auto_connect: true,
            auto_reconnect: true,
            start_with_windows: false,
        }
    }
}

struct Worker {
    stop_tx: watch::Sender<bool>,
    status_rx: std_mpsc::Receiver<String>,
    thread: thread::JoinHandle<()>,
}

struct SelfTunnelApp {
    config: AppConfig,
    status: String,
    worker: Option<Worker>,
    auto_connect_checked: bool,
}

impl SelfTunnelApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        Self {
            config: load_config(),
            status: "Disconnected".to_owned(),
            worker: None,
            auto_connect_checked: false,
        }
    }

    fn start_worker(&mut self) {
        if self.worker.is_some() {
            return;
        }
        if self.config.server_address.trim().is_empty() {
            self.status = "Enter the SelfServer address first".to_owned();
            return;
        }
        if self.config.access_token.len() < 16 {
            self.status = "The access token must contain at least 16 characters".to_owned();
            return;
        }

        save_config(&self.config);
        let config = self.config.clone();
        let (status_tx, status_rx) = std_mpsc::channel::<String>();
        let (stop_tx, mut stop_rx) = watch::channel(false);

        let worker_thread = thread::Builder::new()
            .name("selftunnel-network".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        let _ = status_tx.send(format!("Could not start network runtime: {err}"));
                        return;
                    }
                };

                runtime.block_on(async move {
                    let mut retry_seconds = 2_u64;
                    loop {
                        let _ = status_tx.send("Connecting to SelfServer…".to_owned());
                        let attempt = tokio::select! {
                            result = connect_once(&config, &status_tx) => Some(result),
                            changed = stop_rx.changed() => {
                                if changed.is_err() || *stop_rx.borrow() {
                                    None
                                } else {
                                    continue;
                                }
                            }
                        };

                        let Some(result) = attempt else {
                            break;
                        };

                        match result {
                            Ok(()) => {
                                let _ = status_tx.send("Disconnected".to_owned());
                                retry_seconds = 2;
                            }
                            Err(err) => {
                                warn!(error = %err, "SelfServer connection ended");
                                let _ = status_tx.send(format!("Disconnected: {err}"));
                            }
                        }

                        if !config.auto_reconnect {
                            break;
                        }

                        let _ = status_tx.send(format!("Reconnecting in {retry_seconds}s…"));
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(retry_seconds)) => {}
                            changed = stop_rx.changed() => {
                                if changed.is_err() || *stop_rx.borrow() {
                                    break;
                                }
                            }
                        }
                        retry_seconds = (retry_seconds * 2).min(30);
                    }
                });
            })
            .expect("failed to spawn SelfTunnel network thread");

        self.status = "Connecting to SelfServer…".to_owned();
        self.worker = Some(Worker {
            stop_tx,
            status_rx,
            thread: worker_thread,
        });
    }

    fn stop_worker(&mut self) {
        if let Some(worker) = &self.worker {
            worker.stop_tx.send_replace(true);
        }
        self.status = "Disconnecting…".to_owned();
    }

    fn poll_worker(&mut self) {
        if let Some(worker) = self.worker.as_ref() {
            while let Ok(message) = worker.status_rx.try_recv() {
                self.status = message;
            }
        }
        let finished = self
            .worker
            .as_ref()
            .map(|worker| worker.thread.is_finished())
            .unwrap_or(false);
        if finished {
            if let Some(worker) = self.worker.take() {
                let _ = worker.thread.join();
            }
            if self.status == "Disconnecting…" {
                self.status = "Disconnected".to_owned();
            }
        }
    }

    fn persist_if_changed(&mut self, before: &AppConfig) {
        if &self.config != before {
            save_config(&self.config);
        }
    }
}

impl eframe::App for SelfTunnelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();

        if !self.auto_connect_checked {
            self.auto_connect_checked = true;
            if self.config.auto_connect
                && !self.config.server_address.trim().is_empty()
                && self.config.access_token.len() >= 16
            {
                self.start_worker();
            }
        }

        let before = self.config.clone();
        let connected = self.status.starts_with("Connected");

        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin::same(24)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("SelfTunnel");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (color, label) = if connected {
                            (egui::Color32::from_rgb(72, 201, 128), "CONNECTED")
                        } else {
                            (egui::Color32::from_rgb(160, 160, 170), "DISCONNECTED")
                        };
                        ui.colored_label(color, label);
                    });
                });
                ui.label("A private-to-public TCP tunnel for your server.");
                ui.add_space(18.0);

                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label("CONNECTION");
                    ui.add_space(8.0);

                    ui.label("SelfServer address");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.config.server_address)
                            .hint_text("your-domain.example:9000")
                            .desired_width(f32::INFINITY),
                    );

                    ui.add_space(8.0);
                    ui.label("Access token");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.config.access_token)
                            .password(true)
                            .hint_text("Paste the token configured on SelfServer")
                            .desired_width(f32::INFINITY),
                    );

                    ui.add_space(8.0);
                    ui.label("Local Minecraft server");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.config.local_address)
                            .hint_text("127.0.0.1:25565")
                            .desired_width(f32::INFINITY),
                    );

                    ui.add_space(8.0);
                    ui.label("Custom CA certificate (optional)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.config.ca_certificate)
                            .hint_text("Path to a PEM certificate, or blank for public CAs")
                            .desired_width(f32::INFINITY),
                    );
                });

                ui.add_space(14.0);
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label("BEHAVIOUR");
                    ui.checkbox(&mut self.config.auto_connect, "Connect automatically when SelfTunnel opens");
                    ui.checkbox(&mut self.config.auto_reconnect, "Reconnect automatically if the connection drops");

                    let old_startup = self.config.start_with_windows;
                    if ui
                        .checkbox(&mut self.config.start_with_windows, "Start SelfTunnel with Windows")
                        .changed()
                    {
                        if let Err(err) = set_windows_startup(self.config.start_with_windows) {
                            self.config.start_with_windows = old_startup;
                            self.status = format!("Could not update Windows startup: {err}");
                        } else {
                            self.status = if self.config.start_with_windows {
                                "SelfTunnel will start with Windows".to_owned()
                            } else {
                                "SelfTunnel removed from Windows startup".to_owned()
                            };
                        }
                    }
                });

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    let can_connect = self.worker.is_none();
                    if ui
                        .add_enabled(can_connect, egui::Button::new("Connect").min_size(egui::vec2(120.0, 36.0)))
                        .clicked()
                    {
                        self.start_worker();
                    }
                    if ui
                        .add_enabled(self.worker.is_some(), egui::Button::new("Disconnect").min_size(egui::vec2(120.0, 36.0)))
                        .clicked()
                    {
                        self.stop_worker();
                    }
                });

                ui.add_space(12.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Status");
                    ui.label(&self.status);
                });
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Players connect to your SelfServer public address on TCP port 25565.")
                        .small()
                        .weak(),
                );
            });

        self.persist_if_changed(&before);
        if self.worker.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

async fn connect_once(
    config: &AppConfig,
    status_tx: &std_mpsc::Sender<String>,
) -> Result<()> {
    let tls = connect_tls(config).await?;
    let (mut reader, mut writer) = tokio::io::split(tls);

    send_auth(&mut writer, &config.access_token).await?;
    if !read_auth_result(&mut reader).await? {
        return Err(anyhow!("SelfServer rejected the access token"));
    }

    let _ = status_tx.send("Connected — tunnel is ready".to_owned());
    run_client_session(reader, writer, config.local_address.clone()).await
}

async fn connect_tls(config: &AppConfig) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let (host, port) = split_endpoint(&config.server_address)?;
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    if !config.ca_certificate.trim().is_empty() {
        let file = std::fs::File::open(&config.ca_certificate)
            .with_context(|| format!("cannot open CA certificate: {}", config.ca_certificate))?;
        let mut reader = BufReader::new(file);
        let certificates = rustls_pemfile::certs(&mut reader)
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("could not parse custom CA PEM")?;
        if certificates.is_empty() {
            return Err(anyhow!("custom CA file contains no certificates"));
        }
        for certificate in certificates {
            roots.add(certificate).context("could not add custom CA certificate")?;
        }
    }

    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let addresses = lookup_host((host.as_str(), port))
        .await
        .with_context(|| format!("could not resolve SelfServer host: {host}"))?
        .collect::<Vec<SocketAddr>>();
    if addresses.is_empty() {
        return Err(anyhow!("SelfServer address resolved to no IP addresses"));
    }

    let mut last_error = None;
    let mut connected = None;
    for address in addresses {
        match timeout(Duration::from_secs(8), TcpStream::connect(address)).await {
            Ok(Ok(socket)) => {
                connected = Some(socket);
                break;
            }
            Ok(Err(err)) => last_error = Some(err.to_string()),
            Err(_) => last_error = Some("connection timed out".to_owned()),
        }
    }
    let socket = connected.ok_or_else(|| {
        anyhow!(
            "could not connect to SelfServer: {}",
            last_error.unwrap_or_else(|| "unknown connection error".to_owned())
        )
    })?;
    socket.set_nodelay(true)?;

    let server_name = if let Ok(ip) = host.parse::<IpAddr>() {
        ServerName::IpAddress(ip.into())
    } else {
        ServerName::try_from(host.clone())
            .map_err(|_| anyhow!("invalid TLS server name: {host}"))?
    };
    let connector = TlsConnector::from(std::sync::Arc::new(tls_config));
    let tls = timeout(Duration::from_secs(12), connector.connect(server_name, socket))
        .await
        .context("TLS handshake timed out")?
        .context("TLS certificate validation failed")?;
    Ok(tls)
}

fn split_endpoint(endpoint: &str) -> Result<(String, u16)> {
    let endpoint = endpoint.trim();
    if let Some(bracketed) = endpoint.strip_prefix('[') {
        let (host, tail) = bracketed
            .split_once(']')
            .ok_or_else(|| anyhow!("IPv6 addresses must use [address]:port format"))?;
        let port = tail
            .strip_prefix(':')
            .ok_or_else(|| anyhow!("SelfServer address must include a port"))?
            .parse::<u16>()
            .context("invalid SelfServer port")?;
        if host.is_empty() {
            return Err(anyhow!("SelfServer host cannot be empty"));
        }
        return Ok((host.to_owned(), port));
    }

    let (host, port) = endpoint
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("SelfServer address must include a port, e.g. host:9000"))?;
    if host.is_empty() || host.contains(':') {
        return Err(anyhow!("IPv6 addresses must use [address]:port format"));
    }
    let port = port.parse::<u16>().context("invalid SelfServer port")?;
    Ok((host.to_owned(), port))
}

async fn run_client_session<R, W>(
    mut reader: R,
    mut writer: W,
    local_address: String,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (out_tx, mut out_rx) = mpsc::channel::<Frame>(1024);
    let streams = std::sync::Arc::new(Mutex::new(std::collections::HashMap::<
        u32,
        mpsc::Sender<Vec<u8>>,
    >::new()));

    let mut writer_task = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if let Err(err) = write_frame(&mut writer, &frame).await {
                return Err::<(), std::io::Error>(err);
            }
        }
        Ok(())
    });

    let mut heartbeat = interval(Duration::from_secs(25));
    let result: Result<()> = loop {
        tokio::select! {
            incoming = read_frame(&mut reader) => {
                match incoming {
                    Ok(Frame::Open { id }) => {
                        spawn_local_stream(id, local_address.clone(), streams.clone(), out_tx.clone());
                    }
                    Ok(Frame::Data { id, bytes }) => {
                        let sender = streams.lock().await.get(&id).cloned();
                        if let Some(sender) = sender {
                            if sender.send(bytes).await.is_err() {
                                streams.lock().await.remove(&id);
                                let _ = out_tx.send(Frame::Close { id }).await;
                            }
                        }
                    }
                    Ok(Frame::Close { id }) => {
                        streams.lock().await.remove(&id);
                    }
                    Ok(Frame::Ping) => {
                        if out_tx.send(Frame::Pong).await.is_err() {
                            break Err(anyhow!("control writer stopped"));
                        }
                    }
                    Ok(Frame::Pong) => {}
                    Ok(Frame::OpenOk { .. }) | Ok(Frame::OpenErr { .. }) => {}
                    Err(err) => break Err(err.into()),
                }
            }
            _ = heartbeat.tick() => {
                if out_tx.send(Frame::Ping).await.is_err() {
                    break Err(anyhow!("control writer stopped"));
                }
            }
            writer_result = &mut writer_task => {
                break Err(anyhow!("control writer stopped: {writer_result:?}"));
            }
        }
    };

    streams.lock().await.clear();
    writer_task.abort();
    result
}

fn spawn_local_stream(
    id: u32,
    local_address: String,
    streams: std::sync::Arc<Mutex<std::collections::HashMap<u32, mpsc::Sender<Vec<u8>>>>>,
    out_tx: mpsc::Sender<Frame>,
) {
    tokio::spawn(async move {
        let connection = timeout(Duration::from_secs(8), TcpStream::connect(&local_address)).await;
        let socket = match connection {
            Ok(Ok(socket)) => socket,
            Ok(Err(err)) => {
                let _ = out_tx
                    .send(Frame::OpenErr {
                        id,
                        message: format!("cannot connect to local service: {err}"),
                    })
                    .await;
                return;
            }
            Err(_) => {
                let _ = out_tx
                    .send(Frame::OpenErr {
                        id,
                        message: "local service connection timed out".to_owned(),
                    })
                    .await;
                return;
            }
        };

        if let Err(err) = socket.set_nodelay(true) {
            let _ = out_tx
                .send(Frame::OpenErr {
                    id,
                    message: format!("could not configure local socket: {err}"),
                })
                .await;
            return;
        }

        let (mut local_reader, mut local_writer) = socket.into_split();
        let (inbound_tx, mut inbound_rx) = mpsc::channel::<Vec<u8>>(MAX_STREAM_BUFFER);
        streams.lock().await.insert(id, inbound_tx);

        if out_tx.send(Frame::OpenOk { id }).await.is_err() {
            streams.lock().await.remove(&id);
            return;
        }

        let mut writer_task = tokio::spawn(async move {
            while let Some(bytes) = inbound_rx.recv().await {
                if local_writer.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            let _ = local_writer.shutdown().await;
        });

        let mut buffer = vec![0_u8; 32 * 1024];
        loop {
            tokio::select! {
                read_result = local_reader.read(&mut buffer) => {
                    match read_result {
                        Ok(0) => break,
                        Ok(count) => {
                            if out_tx.send(Frame::Data {
                                id,
                                bytes: buffer[..count].to_vec(),
                            }).await.is_err() {
                                break;
                            }
                        }
                        Err(err) => {
                            warn!(stream_id = id, error = %err, "local service read failed");
                            break;
                        }
                    }
                }
                _ = &mut writer_task => break,
            }
        }

        streams.lock().await.remove(&id);
        let _ = out_tx.send(Frame::Close { id }).await;
        writer_task.abort();
    });
}

fn load_config() -> AppConfig {
    let path = config_path();
    match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    }
}

fn save_config(config: &AppConfig) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            warn!(error = %err, "could not create configuration directory");
            return;
        }
    }
    match toml::to_string_pretty(config) {
        Ok(contents) => {
            if let Err(err) = fs::write(&path, contents) {
                warn!(error = %err, "could not save SelfTunnel configuration");
            }
        }
        Err(err) => warn!(error = %err, "could not serialize SelfTunnel configuration"),
    }
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join("SelfTunnel")
        .join("config.toml")
}

#[cfg(windows)]
fn set_windows_startup(enabled: bool) -> Result<()> {
    use winreg::{
        enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE},
        RegKey,
    };

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu.open_subkey_with_flags(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
        KEY_READ | KEY_WRITE,
    )?;
    if enabled {
        let executable = std::env::current_exe()?;
        let command = format!("\"{}\"", executable.display());
        key.set_value("SelfTunnel", &command)?;
    } else {
        match key.delete_value("SelfTunnel") {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn set_windows_startup(_enabled: bool) -> Result<()> {
    Err(anyhow!("automatic startup registration is currently supported on Windows only"))
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 650.0])
            .with_min_inner_size([480.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "SelfTunnel",
        options,
        Box::new(|cc| Ok(Box::new(SelfTunnelApp::new(cc)))),
    )
}
