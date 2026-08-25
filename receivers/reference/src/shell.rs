//! The native shell: the process that makes `active.json` visible.
//!
//! The receiver's presenter publishes an atomic handoff — a status file and,
//! for a frame, the verified pixels beside it — and deliberately draws
//! nothing. This is the other half: watch the handoff, keep exactly one
//! player process matching it, and never interpret media itself. The player
//! is mpv unless told otherwise; the shell's own judgment ends at argv.
//!
//! The planner is pure — scene in, desire out — and the reconciler restarts
//! the player **only when the desire changes**: a film keeps playing across
//! status rewrites that name the same URL, a frame reloads when its pixels
//! actually changed (the content stamp is part of the desire), and a blank
//! is a killed player and a reason on stderr, never a black window
//! pretending.

use std::path::Path;

use serde::Deserialize;

/// What the handoff currently asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Desire {
    /// Run the player with exactly this argv. The fingerprint decides
    /// restarts, and carries everything a change of which must re-present —
    /// for a frame, the pixels' content stamp; for media, the URL.
    Player {
        argv: Vec<String>,
        fingerprint: String,
    },
    /// No player: the scene is its own message.
    Idle { message: String },
}

#[derive(Deserialize)]
struct Status {
    scene: Scene,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Scene {
    Frame { path: String },
    Media { url: String },
    Blank { reason: String },
    Unsupported,
}

/// The pure planner: one handoff reading to one desire.
///
/// `frame_stamp` is the caller's fact about the frame file (mtime + length,
/// any stable digest of "did the pixels change") because a pure function
/// cannot ask the filesystem and must not pretend it did.
#[must_use]
pub fn desire(output: &Path, status_json: &str, frame_stamp: Option<&str>, player: &str) -> Desire {
    let status: Status = match serde_json::from_str(status_json) {
        Ok(status) => status,
        Err(error) => {
            return Desire::Idle {
                message: format!("handoff is not readable: {error}"),
            }
        }
    };
    match status.scene {
        Scene::Frame { path } => {
            let file = output.join(path);
            let stamp = frame_stamp.unwrap_or("unstamped");
            Desire::Player {
                fingerprint: format!("frame:{stamp}"),
                argv: vec![
                    player.to_string(),
                    "--no-terminal".to_string(),
                    "--fs".to_string(),
                    "--image-display-duration=inf".to_string(),
                    "--loop-file=inf".to_string(),
                    file.to_string_lossy().to_string(),
                ],
            }
        }
        Scene::Media { url } => Desire::Player {
            fingerprint: format!("media:{url}"),
            argv: vec![
                player.to_string(),
                "--no-terminal".to_string(),
                "--fs".to_string(),
                url,
            ],
        },
        Scene::Blank { reason } => Desire::Idle {
            message: format!("blank: {reason}"),
        },
        Scene::Unsupported => Desire::Idle {
            message: "unsupported scene".to_string(),
        },
    }
}

/// What the reconciler asks of the world. The real one runs processes; the
/// tests record.
pub trait Player {
    fn start(&mut self, argv: &[String]);
    fn stop(&mut self);
    /// Whether the started player is still running. A player that died under
    /// an unchanged desire is restarted — resilience, not interpretation.
    fn alive(&mut self) -> bool;
}

/// Keeps exactly one player matching the desire.
#[derive(Default)]
pub struct Reconciler {
    holding: Option<String>,
}

impl Reconciler {
    pub fn step(&mut self, desire: &Desire, player: &mut dyn Player) {
        match desire {
            Desire::Player { argv, fingerprint } => {
                let unchanged = self.holding.as_deref() == Some(fingerprint);
                if unchanged && player.alive() {
                    return;
                }
                player.stop();
                player.start(argv);
                self.holding = Some(fingerprint.clone());
            }
            Desire::Idle { .. } => {
                if self.holding.take().is_some() {
                    player.stop();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn output() -> PathBuf {
        PathBuf::from("/display-output")
    }

    #[derive(Default)]
    struct Recording {
        starts: Vec<Vec<String>>,
        stops: usize,
        running: bool,
    }

    impl Player for Recording {
        fn start(&mut self, argv: &[String]) {
            self.starts.push(argv.to_vec());
            self.running = true;
        }
        fn stop(&mut self) {
            self.stops += 1;
            self.running = false;
        }
        fn alive(&mut self) -> bool {
            self.running
        }
    }

    #[test]
    fn a_frame_desires_the_player_on_the_handoff_pixels() {
        let json = r#"{"scene":{"kind":"frame","path":"frame.png"}}"#;
        let Desire::Player { argv, fingerprint } = desire(&output(), json, Some("m1:100"), "mpv")
        else {
            panic!("a frame is a player scene");
        };
        assert!(argv.last().unwrap().ends_with("frame.png"));
        assert_eq!(fingerprint, "frame:m1:100");
    }

    #[test]
    fn a_film_keeps_playing_across_status_rewrites_naming_the_same_url() {
        let json = r#"{"scene":{"kind":"media","url":"https://c/head/v1/live/t/master.m3u8"}}"#;
        let mut reconciler = Reconciler::default();
        let mut player = Recording::default();
        let wanted = desire(&output(), json, None, "mpv");
        reconciler.step(&wanted, &mut player);
        reconciler.step(&wanted, &mut player);
        reconciler.step(&wanted, &mut player);
        assert_eq!(player.starts.len(), 1, "an unchanged desire never restarts");
    }

    #[test]
    fn changed_pixels_re_present_and_a_blank_kills_the_player() {
        let frame = r#"{"scene":{"kind":"frame","path":"frame.png"}}"#;
        let mut reconciler = Reconciler::default();
        let mut player = Recording::default();
        reconciler.step(
            &desire(&output(), frame, Some("m1:100"), "mpv"),
            &mut player,
        );
        reconciler.step(
            &desire(&output(), frame, Some("m2:104"), "mpv"),
            &mut player,
        );
        assert_eq!(player.starts.len(), 2, "new pixels are a new presentation");

        let blank = r#"{"scene":{"kind":"blank","reason":"revoked"}}"#;
        reconciler.step(&desire(&output(), blank, None, "mpv"), &mut player);
        assert!(!player.running, "a blank is a killed player, not a window");
        // And idling again does not stack stops.
        let stops = player.stops;
        reconciler.step(&desire(&output(), blank, None, "mpv"), &mut player);
        assert_eq!(player.stops, stops);
    }

    #[test]
    fn a_dead_player_under_an_unchanged_desire_is_restarted() {
        let json = r#"{"scene":{"kind":"media","url":"u"}}"#;
        let mut reconciler = Reconciler::default();
        let mut player = Recording::default();
        let wanted = desire(&output(), json, None, "mpv");
        reconciler.step(&wanted, &mut player);
        player.running = false; // the player crashed; the desire did not.
        reconciler.step(&wanted, &mut player);
        assert_eq!(player.starts.len(), 2, "resilience, not interpretation");
    }

    #[test]
    fn an_unreadable_handoff_idles_rather_than_replaying_the_last_scene() {
        let wanted = desire(&output(), "not json", None, "mpv");
        assert!(
            matches!(wanted, Desire::Idle { ref message } if message.contains("not readable")),
            "{wanted:?}"
        );
    }
}
