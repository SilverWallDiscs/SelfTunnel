use eframe::egui;

#[derive(Default)]
struct SelfTunnelApp {
    server_address: String,
    access_token: String,
    local_address: String,
    auto_connect: bool,
    start_with_windows: bool,
    status: String,
}

impl eframe::App for SelfTunnelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("SelfTunnel");
            ui.label("Private server, public connection.");
            ui.add_space(12.0);

            ui.label("SelfServer address");
            ui.text_edit_singleline(&mut self.server_address);

            ui.add_space(8.0);
            ui.label("Access token");
            ui.add(egui::TextEdit::singleline(&mut self.access_token).password(true));

            ui.add_space(8.0);
            ui.label("Local Minecraft server");
            ui.text_edit_singleline(&mut self.local_address);

            ui.add_space(12.0);
            ui.checkbox(&mut self.auto_connect, "Connect automatically when SelfTunnel starts");
            ui.checkbox(&mut self.start_with_windows, "Start SelfTunnel when Windows starts");

            ui.add_space(12.0);
            if ui.button("Connect").clicked() {
                self.status = "Tunnel networking is not implemented yet.".to_owned();
            }
            ui.horizontal(|ui| {
                ui.label("Status:");
                ui.label(if self.status.is_empty() { "Disconnected" } else { &self.status });
            });
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 440.0])
            .with_min_inner_size([460.0, 380.0]),
        ..Default::default()
    };

    eframe::run_native(
        "SelfTunnel",
        options,
        Box::new(|_creation_context| Ok(Box::<SelfTunnelApp>::default())),
    )
}
