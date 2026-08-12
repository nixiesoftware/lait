//! `astrolabe.exe`.
//!
//! `windows_subsystem = "windows"` is why a packaged launch opens no console.
//! It is an attribute rather than a linker flag in CI so that *every* build has
//! the property, including one somebody makes by hand — a release criterion
//! that only holds when the release runner is used is not a property of the
//! program.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use anyhow::{Context, Result};
use astrolabe::model::App;
use astrolabe::ui::Surface;

fn main() -> Result<()> {
    tracing_subscriber_init();

    // Single instance, before anything is drawn: a second launch must hand its
    // work to the first and exit, not race it for the daemon and the state root.
    let _instance = match astrolabe::single_instance::acquire()? {
        astrolabe::single_instance::Outcome::Held(guard) => guard,
        astrolabe::single_instance::Outcome::AlreadyRunning => {
            tracing::info!("another Astrolabe is already running; leaving it to it");
            return Ok(());
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Astrolabe")
            .with_inner_size([1_040.0, 720.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Astrolabe",
        options,
        Box::new(|creation| Ok(Box::new(Shell::new(creation)))),
    )
    .map_err(|error| anyhow::anyhow!("run the Astrolabe window: {error}"))
}

/// The window. It owns the model and draws it, and holds no state of its own
/// beyond which surface is showing.
struct Shell {
    app: App,
    surface: Surface,
}

impl Shell {
    fn new(_creation: &eframe::CreationContext<'_>) -> Self {
        Self {
            app: App::new(),
            surface: Surface::default(),
        }
    }
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        astrolabe::ui::draw(ui, &self.app, &mut self.surface);
    }
}

fn tracing_subscriber_init() {
    // Stderr, not a console: a windows-subsystem program has no console to
    // write to, and asking for one would undo the point of not having it.
    let _ = std::io::Write::flush(&mut std::io::stderr());
}

/// Resolve the `lait.exe` beside us.
///
/// Fixed, relative to the running executable, never chosen by the user and
/// never read from user input. A sidecar path taken from anywhere else is an
/// arbitrary-executable problem wearing a configuration option.
#[allow(dead_code, reason = "wired when the shell starts a supervisor")]
fn sidecar() -> Result<PathBuf> {
    let current = std::env::current_exe().context("locate the running executable")?;
    let name = if cfg!(windows) { "lait.exe" } else { "lait" };
    Ok(current.with_file_name(name))
}
