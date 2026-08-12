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
use astrolabe::runtime::Runtime;
use astrolabe::ui::Surface;
use astrolabe::Config;

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
        Box::new(|creation| Ok(Box::new(Shell::new(creation)?))),
    )
    .map_err(|error| anyhow::anyhow!("run the Astrolabe window: {error}"))
}

/// The window. It owns the model and draws it, and holds no state of its own
/// beyond which surface is showing.
struct Shell {
    app: App,
    surface: Surface,
    /// The background half. Everything slow happens there; the frame loop only
    /// ever drains what it has already produced.
    runtime: Runtime,
}

impl Shell {
    fn new(creation: &eframe::CreationContext<'_>) -> Result<Self> {
        let sidecar = astrolabe::sidecar::resolve()?;
        let state_root = state_root()?;

        // The repaint request is passed in rather than captured inside the
        // runtime, so the background half has no opinion about what a frame
        // loop is and can be driven by a test that has none.
        let context = creation.egui_ctx.clone();
        let runtime = Runtime::start(Config::new(state_root, sidecar), move || {
            context.request_repaint();
        })?;

        Ok(Self {
            app: App::new(),
            surface: Surface::default(),
            runtime,
        })
    }
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Applied at the top of the frame, so everything drawn below sees the
        // same state — a surface that drained mid-frame could show two
        // different readings of the same moment.
        self.runtime.drain_into(&mut self.app);
        astrolabe::ui::draw(ui, &self.app, &mut self.surface);
    }
}

/// Where this client keeps what it manages.
///
/// Under the user's local data directory, not beside the executable: a program
/// directory may be read-only, may be shared between users, and is replaced
/// wholesale by an upgrade.
fn state_root() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .context("locate a local data directory for the managed state root")?;
    let root = base.join("Astrolabe").join("devices");
    std::fs::create_dir_all(&root)
        .with_context(|| format!("create the managed state root {}", root.display()))?;
    Ok(root)
}

fn tracing_subscriber_init() {
    // Stderr, not a console: a windows-subsystem program has no console to
    // write to, and asking for one would undo the point of not having it.
    let _ = std::io::Write::flush(&mut std::io::stderr());
}
