//! "Let your Agents Control RustGIS" (desktop), in the AI Assistant settings. Ticking it turns
//! on the MCP relay ([`rustgis_assistant::relay`]): AI agents on this computer that connect
//! (Claude Code, Codex, Cursor…) work on the map open here, with file and network access, no
//! `--yes` needed. While it's on, the section shows what an agent needs to connect, and nothing
//! else. The choice is remembered: the relay starts with the app.

use egui::RichText;
use serde_json::json;

use crate::RustGisApp;
use crate::theme::Tokens;

/// The preference that remembers the choice.
pub const PREF: &str = "agents_control";

#[derive(Default)]
pub struct ConnectState {
    /// The choice, once read from the preferences.
    on: Option<bool>,
    /// The relay was started for a remembered choice (once, at launch).
    resumed: bool,
    /// Why the relay couldn't start, when it couldn't.
    error: Option<String>,
}

/// Whether the user lets agents control RustGIS.
pub fn enabled(app: &RustGisApp) -> bool {
    app.connect.on.unwrap_or_else(|| app.services.load_pref.as_ref().and_then(|l| l(PREF)).as_deref() == Some("on"))
}

/// At launch, starts the relay when the user had agents turned on.
pub fn resume(app: &mut RustGisApp) {
    if std::mem::replace(&mut app.connect.resumed, true) || !enabled(app) || app.relay.is_some() {
        return;
    }
    if let Err(e) = app.run("ui.mcpRelay", json!({})) {
        app.connect.error = Some(e);
    }
}

/// `rustgis-cli` beside the app, or its name (for a PATH lookup) when it isn't there.
fn cli() -> String {
    let name = if cfg!(windows) { "rustgis-cli.exe" } else { "rustgis-cli" };
    std::env::current_exe()
        .ok()
        .map(|p| p.with_file_name(name))
        .filter(|p| p.is_file())
        .map_or(name.to_string(), |p| p.display().to_string())
}

/// A line to copy: shown in monospace, with a Copy button.
fn copyable(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(RichText::new(text).monospace().size(11.5)).wrap());
        if ui.small_button(tl!("Copy")).clicked() {
            ui.ctx().copy_text(text.to_string());
        }
    });
}

/// The section in the assistant settings.
pub fn section(app: &mut RustGisApp, ui: &mut egui::Ui, t: &Tokens) {
    let mut on = enabled(app);
    let hint = tl!(
        "Agents on this computer that connect (Claude Code, Codex, Cursor and others) work on the map open here, with RustGIS's tools: they can open and save files and use the web. Scripts and plugin management stay off."
    );
    if ui.checkbox(&mut on, tl!("Let your Agents Control RustGIS")).on_hover_text(hint).changed() {
        app.connect.on = Some(on);
        app.connect.error = None;
        if let Some(save) = &app.services.save_pref {
            save(PREF, if on { "on" } else { "off" });
        }
        let started = if on { app.run("ui.mcpRelay", json!({})) } else { app.run("ui.mcpRelay", json!({"stop": true})) };
        if let Err(e) = started {
            app.connect.error = Some(e);
        }
        if let Some(relay) = &app.relay {
            relay.set_allow_access(on);
        }
    }
    if let Some(e) = &app.connect.error {
        ui.label(RichText::new(e).small().color(t.red));
    }
    let (true, Some(port)) = (on, app.relay.as_ref().map(|r| r.port())) else { return };
    // What an agent needs: RustGIS's MCP server, attached to this app.
    let cli = cli();
    let mut args = vec!["mcp".to_string(), "--attach".to_string()];
    if port != rustgis_assistant::relay::DEFAULT_PORT {
        args.push(port.to_string());
    }
    let config = serde_json::to_string_pretty(&json!({"mcpServers": {"rustgis-app": {"command": cli, "args": args}}})).unwrap_or_default();
    ui.label(RichText::new(tl!("Add this MCP server to your agent:")).small());
    copyable(ui, &config);
    ui.label(RichText::new(tl!("Or run, for Claude Code or Codex:")).small());
    let quoted = if cli.contains(' ') { format!("\"{cli}\"") } else { cli.clone() };
    for agent in ["claude", "codex"] {
        copyable(ui, &format!("{agent} mcp add rustgis-app -- {quoted} {}", args.join(" ")));
    }
}
