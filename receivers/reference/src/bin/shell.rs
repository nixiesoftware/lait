//! `astrolabe-display-shell`: keep one player matching the receiver's handoff.
//!
//! ```text
//! astrolabe-display-shell --output display-output [--player mpv]
//! ```
//!
//! Polls the handoff — the receiver replaces it atomically, so a read is
//! whole or last — and reconciles a single player process against it. See
//! `shell.rs` for the rules; this file is only the clock and the real
//! processes.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use astrolabe_display_reference::shell::{desire, Desire, Player, Reconciler};

struct RealPlayer {
    child: Option<Child>,
}

impl Player for RealPlayer {
    fn start(&mut self, argv: &[String]) {
        let Some((program, args)) = argv.split_first() else {
            return;
        };
        match Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .spawn()
        {
            Ok(child) => self.child = Some(child),
            Err(error) => eprintln!("shell: could not start {program}: {error}"),
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn alive(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }
}

fn main() -> Result<()> {
    let mut output: Option<PathBuf> = None;
    let mut player = "mpv".to_string();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--output" => output = arguments.next().map(PathBuf::from),
            "--player" => {
                if let Some(value) = arguments.next() {
                    player = value;
                }
            }
            other => bail!("unknown argument '{other}' (expected --output <dir> [--player <bin>])"),
        }
    }
    let output =
        output.context("--output <dir> is required: the receiver's presentation directory")?;
    let status_path = output.join("active.json");

    let mut reconciler = Reconciler::default();
    let mut real = RealPlayer { child: None };
    let mut last_idle: Option<String> = None;
    loop {
        let wanted = match std::fs::read_to_string(&status_path) {
            Ok(json) => {
                // The pixels' content stamp: mtime + length, which is what
                // changes when the receiver atomically replaces the frame.
                let stamp = std::fs::metadata(output.join("frame.png"))
                    .ok()
                    .map(|meta| {
                        let modified = meta
                            .modified()
                            .ok()
                            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0, |at| at.as_millis());
                        format!("{modified}:{}", meta.len())
                    });
                desire(&output, &json, stamp.as_deref(), &player)
            }
            Err(_) => Desire::Idle {
                message: "no handoff yet".to_string(),
            },
        };
        if let Desire::Idle { message } = &wanted {
            if last_idle.as_deref() != Some(message) {
                eprintln!("shell: {message}");
                last_idle = Some(message.clone());
            }
        } else {
            last_idle = None;
        }
        reconciler.step(&wanted, &mut real);
        std::thread::sleep(Duration::from_millis(250));
    }
}
