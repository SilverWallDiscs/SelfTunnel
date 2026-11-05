use std::{
    collections::HashMap,
    env,
    fs::File,
    io::BufReader,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use selfprotocol::{
    read_auth, read_frame, send_auth_result, write_frame, Frame,
};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, watch, Mutex},
    time::{interval, timeout},
};
use tokio_rustls::{rustls, TlsAcceptor};
use tracing::{error, info, warn};

const CONTROL_DEFAULT: &str = "0.0.0.0:9000";
const PUBLIC_DEFAULT: &str = "0.0.0.0:25565";
const OPEN_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_STREAM_BUFFER: usize = 32;

struct StreamEntry {
    inbound_tx: mpsc::Sender<Vec<u8>>,
    opened: Option<oneshot::Sender<std::result::Result<(), String>>>,
}

#[derive(Clone)]
struct ClientSession {
    out_tx: mpsc::Sender<Frame>,
    streams: Arc<Mutex<HashMap<u32, StreamEntry>>>,
    next_stream: Arc<AtomicU32>,
    alive: watch::Receiver<bool>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let control_bind = env::var("SELFTUNNEL_CONTROL_BIND")
        .unwrap_or_else(|_| CONTROL_DEFAULT.to_owned())
        .parse::<SocketAddr>()
        .context("invalid SELFTUNNEL_CONTROL_BIND")?;
    let public_bind = env::var("SELFTUNNEL_PUBLIC_BIND")
        .unwrap_or_else(|_| PUBLIC_DEFAULT.to_owned())
        .parse::<SocketAddr>()
        .context("invalid SELFTUNNEL_PUBLIC_BIND")?;
    let token = env::var("SELFTUNNEL_TOKEN")
        .context("set SELFTUNNEL_TOKEN to a random secret of at least 16 characters")?;
    if token.len() < 16 {
        return Err(anyhow!("SELFTUNNEL_TOKEN must contain at least 16 characters"));
    }

    let cert_path = env::var("SELFTUNNEL_TLS_CERT")
        .context("set SELFTUNNEL_TLS_CERT to your TLS certificate chain PEM")?;
    let key_path = env::var("SELFTUNNEL_TLS_KEY")
        .context("set SELFTUNNEL_TLS_KEY to your TLS private key PEM")?;
    let acceptor = load_tls_acceptor(&cert_path, &key_path)?;

    let control_listener = TcpListener::bind(control_bind)
        .await
        .with_context(|| format!("failed to bind control listener at {control_bind}"))?;
    let public_listener = TcpListener::bind(public_bind)
        .await
        .with_context(|| format!("failed to bind public listener at {public_bind}"))?;

    info!("SelfServer {}", env!("CARGO_PKG_VERSION"));
    info!("Control listener: {control_bind}");
    info!("Public TCP listener: {public_bind}");
    info!("Waiting for one authenticated SelfTunnel client");

    let (session_tx, session_rx) = watch::channel::<Option<ClientSession>>(None);
    let session_gate = Arc::new(Mutex::new(()));

    let public_state = session_rx.clone();
    tokio::spawn(async move {
        loop {
            match public_listener.accept().await {
                Ok((socket, peer)) => {
                    let state = public_state.clone();
                    tokio::spawn(async move {
                        if let Err(err) = handle_public_connection(socket, peer, state).await {
                            warn!(%peer, error = %err, "public connection ended");
                        }
                    });
                }
                Err(err) => {
                    error!(error = %err, "public listener accept failed");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });

    loop {
        let (socket, peer) = control_listener.accept().await?;
        info!(%peer, "incoming SelfTunnel control connection");

        let expected_token = token.clone();
        let tls_acceptor = acceptor.clone();
        let state = session_tx.clone();
        let gate = session_gate.clone();

        tokio::spawn(async move {
            let _guard = gate.lock().await;
            if let Err(err) = run_control_session(
                socket,
                peer,
                tls_acceptor,
                expected_token,
                state,
            )
            .await
            {
                warn!(%peer, error = %err, "SelfTunnel session ended");
            }
        });
    }
}

fn load_tls_acceptor(cert_path: &str, key_path: &str) -> Result<TlsAcceptor> {
    let mut cert_reader = BufReader::new(
        File::open(cert_path).with_context(|| format!("cannot open TLS certificate: {cert_path}"))?,
    );
    let certs = rustls_pemfile::certs(&mut cert_reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("could not parse TLS certificate PEM")?;
    if certs.is_empty() {
        return Err(anyhow!("TLS certificate file contains no certificates"));
    }

    let mut key_reader = BufReader::new(
        File::open(key_path).with_context(|| format!("cannot open TLS private key: {key_path}"))?,
    );
    let key = rustls_pemfile::private_key(&mut key_reader)
        .context("could not parse TLS private key PEM")?
        .ok_or_else(|| anyhow!("TLS private key file contains no supported private key"))?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("TLS certificate and private key do not match")?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

async fn run_control_session(
    socket: TcpStream,
    peer: SocketAddr,
    acceptor: TlsAcceptor,
    expected_token: String,
    session_state: watch::Sender<Option<ClientSession>>,
) -> Result<()> {
    let tls = timeout(Duration::from_secs(15), acceptor.accept(socket))
        .await
        .context("TLS handshake timed out")?
        .context("TLS handshake failed")?;

    let (mut reader, mut writer) = tokio::io::split(tls);
    let supplied_token = timeout(Duration::from_secs(10), read_auth(&mut reader))
        .await
        .context("authentication handshake timed out")??;

    let accepted = bool::from(supplied_token.as_slice().ct_eq(expected_token.as_bytes()));
    send_auth_result(&mut writer, accepted).await?;
    if !accepted {
        warn!(%peer, "rejected invalid access token");
        return Ok(());
    }

    let (out_tx, mut out_rx) = mpsc::channel::<Frame>(1024);
    let streams = Arc::new(Mutex::new(HashMap::<u32, StreamEntry>::new()));
    let (alive_tx, alive_rx) = watch::channel(false);
    let session = ClientSession {
        out_tx: out_tx.clone(),
        streams: streams.clone(),
        next_stream: Arc::new(AtomicU32::new(1)),
        alive: alive_rx.clone(),
    };

    let writer_task = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if let Err(err) = write_frame(&mut writer, &frame).await {
                return Err::<(), std::io::Error>(err);
            }
        }
        Ok(())
    });

    session_state.send_replace(Some(session));
    info!(%peer, "SelfTunnel client authenticated and ready");

    let mut heartbeat = interval(Duration::from_secs(25));
    let result: Result<()> = loop {
        tokio::select! {
            incoming = read_frame(&mut reader) => {
                match incoming {
                    Ok(frame) => {
                        if let Err(err) = handle_control_frame(frame, &out_tx, &streams).await {
                            break Err(err);
                        }
                    }
                    Err(err) => break Err(err.into()),
                }
            }
            _ = heartbeat.tick() => {
                if out_tx.send(Frame::Ping).await.is_err() {
                    break Err(anyhow!("control writer stopped"));
                }
            }
            writer_result = &mut tokio::time::sleep(Duration::from_millis(0)), if writer_task.is_finished() => {
                let _ = writer_result;
                break Err(anyhow!("control writer stopped"));
            }
        }
    };

    alive_tx.send_replace(true);
    session_state.send_replace(None);
    writer_task.abort();
    result
}

async fn handle_control_frame(
    frame: Frame,
    out_tx: &mpsc::Sender<Frame>,
    streams: &Arc<Mutex<HashMap<u32, StreamEntry>>>,
) -> Result<()> {
    match frame {
        Frame::OpenOk { id } => {
            let mut map = streams.lock().await;
            if let Some(entry) = map.get_mut(&id) {
                if let Some(opened) = entry.opened.take() {
                    let _ = opened.send(Ok(()));
                }
            }
        }
        Frame::OpenErr { id, message } => {
            if let Some(mut entry) = streams.lock().await.remove(&id) {
                if let Some(opened) = entry.opened.take() {
                    let _ = opened.send(Err(message));
                }
            }
        }
        Frame::Data { id, bytes } => {
            let sender = streams.lock().await.get(&id).map(|entry| entry.inbound_tx.clone());
            if let Some(sender) = sender {
                if sender.send(bytes).await.is_err() {
                    streams.lock().await.remove(&id);
                    let _ = out_tx.send(Frame::Close { id }).await;
                }
            }
        }
        Frame::Close { id } => {
            if let Some(mut entry) = streams.lock().await.remove(&id) {
                if let Some(opened) = entry.opened.take() {
                    let _ = opened.send(Err("remote closed the connection".to_owned()));
                }
            }
        }
        Frame::Ping => {
            out_tx.send(Frame::Pong).await.map_err(|_| anyhow!("control writer stopped"))?;
        }
        Frame::Pong => {}
        Frame::Open { .. } => return Err(anyhow!("client sent an invalid OPEN frame")),
    }
    Ok(())
}

async fn handle_public_connection(
    socket: TcpStream,
    peer: SocketAddr,
    mut session_state: watch::Receiver<Option<ClientSession>>,
) -> Result<()> {
    let session = session_state.borrow_and_update().clone();
    let Some(session) = session else {
        return Err(anyhow!("no SelfTunnel client is connected"));
    };
    if *session.alive.borrow() {
        return Err(anyhow!("SelfTunnel client is disconnected"));
    }

    let id = session.next_stream.fetch_add(1, Ordering::Relaxed);
    if id == 0 {
        return Err(anyhow!("stream identifier space exhausted"));
    }

    let (mut local_reader, local_writer) = socket.into_split();
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<Vec<u8>>(MAX_STREAM_BUFFER);
    let (opened_tx, opened_rx) = oneshot::channel();

    session.streams.lock().await.insert(
        id,
        StreamEntry {
            inbound_tx,
            opened: Some(opened_tx),
        },
    );

    session
        .out_tx
        .send(Frame::Open { id })
        .await
        .map_err(|_| anyhow!("SelfTunnel control connection is closed"))?;

    let mut alive = session.alive.clone();
    let open_result = tokio::select! {
        result = timeout(OPEN_TIMEOUT, opened_rx) => {
            match result {
                Ok(Ok(Ok(()))) => Ok(()),
                Ok(Ok(Err(message))) => Err(anyhow!(message)),
                Ok(Err(_)) => Err(anyhow!("SelfTunnel client dropped the stream request")),
                Err(_) => Err(anyhow!("timed out waiting for the local Minecraft server")),
            }
        }
        _ = alive.changed() => Err(anyhow!("SelfTunnel client disconnected")),
    };

    if let Err(err) = open_result {
        session.streams.lock().await.remove(&id);
        let _ = session.out_tx.send(Frame::Close { id }).await;
        return Err(err);
    }

    let mut local_writer = local_writer;
    let mut writer_task = tokio::spawn(async move {
        while let Some(bytes) = inbound_rx.recv().await {
            if local_writer.write_all(&bytes).await.is_err() {
                break;
            }
        }
        let _ = local_writer.shutdown().await;
    });

    info!(%peer, stream_id = id, "Minecraft connection opened");
    let mut buffer = vec![0_u8; 32 * 1024];

    loop {
        tokio::select! {
            read_result = local_reader.read(&mut buffer) => {
                match read_result {
                    Ok(0) => break,
                    Ok(count) => {
                        if session.out_tx.send(Frame::Data {
                            id,
                            bytes: buffer[..count].to_vec(),
                        }).await.is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        warn!(%peer, stream_id = id, error = %err, "player socket read failed");
                        break;
                    }
                }
            }
            _ = &mut writer_task => break,
            _ = alive.changed() => break,
        }
    }

    session.streams.lock().await.remove(&id);
    let _ = session.out_tx.send(Frame::Close { id }).await;
    writer_task.abort();
    info!(%peer, stream_id = id, "Minecraft connection closed");
    Ok(())
}
