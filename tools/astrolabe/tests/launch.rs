//! `Open`, against the real binary.
//!
//! Every other test in this crate proves a rule without a process. This one
//! proves the seam that has no rule in it and three places to be wrong: the
//! supervisor spawns `lait` with the flags it thinks the launcher takes, the
//! launcher accepts them, the head announces an address before it accepts, and
//! the address it announced answers `/api/launch` with a credential that is
//! worth exactly one use.
//!
//! It exists because that seam has already been wrong once. `start_head` passed
//! `--home` to a launcher mode that did not take it, so the head exited before
//! printing anything and the supervisor reported "head exited before it
//! announced an address" — a message that describes the symptom and names
//! nothing. No unit test could have caught it, because every part was correct
//! on its own.
//!
//! ## What it does not do
//!
//! It stops one call short of `open_world`, which hands the URL to the person's
//! browser. A test that opened a browser window on a CI runner — or on the
//! machine of whoever is running the suite — would be a test that costs more
//! than it proves.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use astrolabe::client::http::{post_json, Head};
use astrolabe::client::{display::DisplayAssignmentInput, Client};
use astrolabe::Config;

/// The `lait` this build produced, if it is there.
///
/// Found relative to the running test binary rather than by walking the
/// workspace: `target/<profile>/deps/<test>-<hash>.exe` puts it two levels up,
/// and that holds for every profile without knowing which one is in use.
///
/// Freshness is the caller's: nextest builds test binaries, never workspace
/// bins, so what sits here is whatever the last `cargo build` left — run
/// `cargo build --workspace --all-targets` first, as CI does. A stale binary
/// fails these tests in ways that name everything except its own age; a
/// stale receiver once read as a broken media pipeline for most of a day.
fn sidecar() -> Option<PathBuf> {
    built_binary("lait")
}

fn reference_receiver() -> Option<PathBuf> {
    built_binary("astrolabe-display-reference")
}

fn built_binary(name: &str) -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let profile = current.parent()?.parent()?;
    let name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let candidate = profile.join(name);
    candidate.is_file().then_some(candidate)
}

/// Assemble the same immutable first-party seed releases the Tauri bundle
/// carries. The launch seam must exercise the product-blind sidecar with real
/// selected runners; linking a test package into the host would conceal a
/// missing resource or installer handoff.
fn stage_bundled_worlds(root: &Path) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let exe = if cfg!(windows) { ".exe" } else { "" };
    for (id, package, template, runner) in [
        (
            "com.lait.issues",
            "products/issues/Cargo.toml",
            "products/issues-runner/world.json.template",
            "lait-world-issues",
        ),
        (
            "com.lait.signage",
            "products/signage/Cargo.toml",
            "products/signage-runner/world.json.template",
            "lait-world-signage",
        ),
    ] {
        let version = package_version(&repo.join(package));
        let release = root.join(id).join(&version);
        std::fs::create_dir_all(release.join("bin")).expect("create seed World release");
        let binary = built_binary(runner).unwrap_or_else(|| {
            panic!(
                "no {runner} binary beside the test binary; build the workspace bins before the launch suite"
            )
        });
        std::fs::copy(binary, release.join("bin").join(format!("{runner}{exe}")))
            .expect("copy seed World runner");
        let declaration = std::fs::read_to_string(repo.join(template))
            .expect("read seed World declaration")
            .replace("${VERSION}", &version)
            .replace("${EXE}", exe);
        std::fs::write(release.join("world.json"), declaration)
            .expect("write seed World declaration");

        if id == "com.lait.issues" {
            copy_tree(&repo.join("products/issues-app/assets/web"), &release);
            let art = release.join("art");
            std::fs::create_dir_all(&art).expect("create Issues artwork directory");
            std::fs::copy(
                repo.join("products/issues-app/assets/mark.png"),
                art.join("mark.png"),
            )
            .expect("copy Issues mark");
            std::fs::copy(
                repo.join("products/issues-app/assets/hero.png"),
                art.join("hero.png"),
            )
            .expect("copy Issues hero");
        }
    }
}

fn package_version(manifest: &Path) -> String {
    std::fs::read_to_string(manifest)
        .expect("read product manifest")
        .lines()
        .find_map(|line| {
            line.strip_prefix("version")?
                .trim_start()
                .strip_prefix('=')?
                .trim()
                .strip_prefix('"')?
                .strip_suffix('"')
                .map(str::to_owned)
        })
        .expect("product manifest package version")
}

fn copy_tree(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).expect("create seed World asset directory");
    for entry in std::fs::read_dir(source).expect("read seed World asset directory") {
        let entry = entry.expect("read seed World asset entry");
        let destination = target.join(entry.file_name());
        if entry
            .file_type()
            .expect("read seed World asset type")
            .is_dir()
        {
            copy_tree(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), destination).expect("copy seed World asset");
        }
    }
}

struct OwnedReceiver(Child);

impl Drop for OwnedReceiver {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Stops the identity daemon however the test ends, panic included.
///
/// `stop_daemon` at the foot of the body only runs when the body reaches it,
/// and a panic is how this test reports every failure. The daemon it started
/// then outlived it still holding the display coordinator's fixed port, so
/// nextest's retry hit the port guard 23ms in and reported "something else on
/// this machine holds it" — which was the previous attempt of this same test,
/// and read as a dirty machine. The retry could not have passed, and the
/// failure the log ended on was never the one worth reading.
///
/// Its own thread and its own runtime: a `Drop` running during an unwind cannot
/// await, and must not panic — a panic here would abort the process and take
/// the real failure with it.
struct DaemonStopped(PathBuf);

impl Drop for DaemonStopped {
    fn drop(&mut self) {
        let home = self.0.clone();
        let _ = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                stop_daemon(&home).await;
                for _ in 0..100 {
                    let selection = lait::config::Selection::for_identity(&home);
                    let gone = match lait::daemon::Client::for_selection(&selection) {
                        Ok(daemon) => {
                            matches!(daemon.probe().await, lait::control::Probe::Absent)
                                && lait::config::acquire_daemon_lock(daemon.home()).is_ok()
                        }
                        Err(_) => true,
                    };
                    if gone {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            });
        })
        .join();
    }
}

async fn wait_for_daemon_stop(home: &Path) {
    for _ in 0..100 {
        let selection = lait::config::Selection::for_identity(home);
        let stopped = match lait::daemon::Client::for_selection(&selection) {
            Ok(daemon) if matches!(daemon.probe().await, lait::control::Probe::Absent) => {
                // The endpoint closes before the process has necessarily
                // released its single-instance lock while active Orbits drain.
                // A replacement is safe only after both are gone.
                lait::config::acquire_daemon_lock(daemon.home()).is_ok()
            }
            Err(_) => true,
            _ => false,
        };
        if stopped {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("identity daemon did not stop within its process bound");
}

async fn wait_for_pairing(client: &Client) -> String {
    for _ in 0..100 {
        let display = client
            .display_status()
            .await
            .expect("read display pairing status");
        if let Some(pairing) = display.pending_pairings.first() {
            return pairing.pairing.clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("reference receiver did not open a pairing within ten seconds");
}

async fn wait_for_receiver(client: &Client) -> String {
    for _ in 0..100 {
        let display = client
            .display_status()
            .await
            .expect("read enrolled display status");
        if let Some(device) = display.devices.first() {
            if display.pending_pairings.is_empty() {
                return device.device.clone();
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("reference receiver did not complete enrollment within ten seconds");
}

async fn wait_for_new_receiver(client: &Client, existing: &str) -> String {
    for _ in 0..100 {
        let display = client
            .display_status()
            .await
            .expect("read enrolled display status");
        if let Some(device) = display
            .devices
            .iter()
            .find(|device| device.device != existing)
        {
            if display.pending_pairings.is_empty() {
                return device.device.clone();
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("second reference receiver did not complete enrollment within ten seconds");
}

async fn wait_for_unassigned(path: &Path, device: &str) {
    for _ in 0..150 {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(status) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if status["device"] == device && status["scene"]["reason"] == "unassigned" {
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("enrolled receiver never presented its authenticated unassigned state");
}

async fn wait_for_assigned(
    path: &Path,
    assignment: &str,
    program: &str,
    identity: &Path,
) -> String {
    let mut last = "active receiver state was absent".to_string();
    for _ in 0..200 {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(status) => {
                    last = status.to_string();
                    if let (Some(revision), Some(item)) =
                        (status["revision"].as_str(), status["item"].as_str())
                    {
                        if status["assignment"] == assignment
                            && status["program"] == program
                            && status["scene"]["kind"] == "frame"
                            && !revision.is_empty()
                            && !item.is_empty()
                        {
                            return revision.to_owned();
                        }
                    }
                }
                Err(error) => last = format!("invalid receiver state: {error}"),
            },
            Err(error) => last = format!("receiver state unavailable: {error}"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let log = daemon_log_tail(identity, 80);
    panic!(
        "assigned receiver never presented the compiled Signage frame\n\
         --- last state at {} ---\n{last}\n--- daemon log tail ---\n{log}",
        path.display()
    );
}

async fn wait_for_revision_change(
    path: &Path,
    assignment: &str,
    program: &str,
    prior_revision: &str,
    phase: &str,
) -> String {
    for _ in 0..200 {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(status) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let (Some(revision), Some(item)) =
                    (status["revision"].as_str(), status["item"].as_str())
                {
                    if status["assignment"] == assignment
                        && status["program"] == program
                        && status["scene"]["kind"] == "frame"
                        && revision != prior_revision
                        && !item.is_empty()
                    {
                        return revision.to_owned();
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("assigned receiver never received the {phase} semantic revision");
}

/// Wait for the coordinator to observe this receiver displaying `revision`.
///
/// Matched on the revision alone, and deliberately. The revision names the
/// program that was presented and does not move; the *item* is whichever frame
/// of that program is on screen right now, and a looping program advances every
/// couple of seconds by design. Requiring a specific item asserted something
/// that is only true for one item's duration at a time — and it was a snapshot
/// taken from the receiver's own `active.json` before this call, so the
/// receiver had usually moved on before the coordinator observed anything.
///
/// That failed as *"never observed health"*, which is the opposite of what was
/// happening: the receiver was online, displaying, and reporting a perfectly
/// correct different item.
///
/// The item is still checked — for being *present*, which is the part that
/// would break if the receiver stopped tracking its position.
async fn wait_for_health(client: &Client, device: &str, revision: &str) {
    // The frame programs in this test stage one or two cards.
    wait_for_health_staged(client, device, revision, 1..=2).await;
}

/// Health, with the staging the current program actually implies.
///
/// A frame program stages its cards; a media program stages a grant and no
/// bytes at all — staging is a frame budget, and a film that staged nothing is
/// the design working, not a receiver failing to load.
async fn wait_for_health_staged(
    client: &Client,
    device: &str,
    revision: &str,
    staged: std::ops::RangeInclusive<u16>,
) {
    let mut last = String::from("the coordinator was never reached");
    for _ in 0..200 {
        let display = client.display_status().await.expect("read receiver health");
        if let Some(health) = display
            .devices
            .iter()
            .find(|row| row.device == device)
            .and_then(|row| row.health.as_ref())
            .filter(|health| health.revision == revision && !health.current_item.is_empty())
        {
            assert_eq!(health.connection, "online");
            assert_eq!(health.playback, "displaying");
            assert_eq!(health.last_error, "none");
            assert!(
                staged.contains(&health.staged_items),
                "receiver staged {} items where {staged:?} were expected",
                health.staged_items
            );
            assert_eq!(
                health.staged_bytes > 0,
                *staged.start() > 0,
                "staged bytes must match staged items: a media grant is not bytes"
            );
            assert!(health.pipeline_unobservable);
            return;
        }
        last = match display.devices.iter().find(|row| row.device == device) {
            None => format!(
                "the coordinator lists no device {device} at all (it knows: [{known}])",
                known = display
                    .devices
                    .iter()
                    .map(|row| row.device.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Some(row) => match row.health.as_ref() {
                None => format!("device {device} is paired but has reported no health yet"),
                Some(health) => format!(
                    "device {device} last reported revision {seen_revision}/item {seen_item} \
                     (connection {connection}, playback {playback}, last_error {error}) \
                     while this waited for revision {revision}",
                    seen_revision = health.revision,
                    seen_item = health.current_item,
                    connection = health.connection,
                    playback = health.playback,
                    error = health.last_error,
                ),
            },
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Three different bugs used to arrive as this one sentence: a receiver that
    // never paired, one that paired and went silent, and one stuck presenting an
    // older revision. Only the third is a Signage bug, and the message could not
    // tell them apart — so a nightly failure on a platform nobody has in front of
    // them named nothing and cost a bisect.
    panic!(
        "coordinator never observed health for the presented Signage revision \
         after 20s. Last observation: {last}"
    );
}

async fn wait_for_group_boundary(first: &Path, second: &Path, group: &str) {
    let read = |path: &Path| {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    };
    let first_frame = first.with_file_name("frame.png");
    let second_frame = second.with_file_name("frame.png");
    let mut initial = None;
    for _ in 0..400 {
        if let (Some(first), Some(second)) = (read(first), read(second)) {
            let aligned = first["sync"]["group"] == group
                && second["sync"]["group"] == group
                && first["sync"]["mode"] == "stay_in_sync"
                && second["sync"]["mode"] == "stay_in_sync";
            if aligned {
                if let (Ok(first_bytes), Ok(second_bytes)) =
                    (std::fs::read(&first_frame), std::fs::read(&second_frame))
                {
                    if first_bytes == second_bytes {
                        initial = Some(first_bytes);
                        break;
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let initial = initial.expect("two receivers never adopted one boundary-sync target");

    let observed = std::time::Instant::now();
    let mut first_boundary = None;
    let mut second_boundary = None;
    for _ in 0..400 {
        let first_bytes = std::fs::read(&first_frame).ok();
        let second_bytes = std::fs::read(&second_frame).ok();
        if first_boundary.is_none() && first_bytes.as_ref().is_some_and(|bytes| bytes != &initial) {
            first_boundary = Some(observed.elapsed());
        }
        if second_boundary.is_none() && second_bytes.as_ref().is_some_and(|bytes| bytes != &initial)
        {
            second_boundary = Some(observed.elapsed());
        }
        if let (Some(first_at), Some(second_at), Some(first_bytes), Some(second_bytes)) =
            (first_boundary, second_boundary, first_bytes, second_bytes)
        {
            assert_eq!(
                first_bytes, second_bytes,
                "sync group advanced to different presented frames"
            );
            assert!(
                first_at.abs_diff(second_at) <= Duration::from_millis(500),
                "boundary-synced receivers drifted by more than 500 ms"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("two assigned receivers never crossed a shared program boundary");
}

/// Wait for the receiver to present a media handoff, and return it.
///
/// The URL in `active.json` is the proof of the whole stored chain: the
/// receiver minted a real ticket, which the coordinator honoured only after it
/// walked the uploaded container's own bytes over the content plane, planned
/// every segment, and installed the presentation. Nothing in this test fetched
/// a byte of media by hand.
async fn wait_for_media(
    path: &Path,
    assignment: &str,
    program: &str,
    identity: &Path,
) -> (String, String) {
    for _ in 0..200 {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(status) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if status["assignment"] == assignment
                    && status["program"] == program
                    && status["scene"]["kind"] == "media"
                {
                    if let (Some(revision), Some(url)) =
                        (status["revision"].as_str(), status["scene"]["url"].as_str())
                    {
                        if !revision.is_empty() && !url.is_empty() {
                            return (revision.to_owned(), url.to_owned());
                        }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Named, not shrugged: where the receiver actually stood, and the
    // daemon's own account — the chain has five quiet places to stall.
    let last = std::fs::read_to_string(path).unwrap_or_else(|_| "<no active.json>".into());
    let log = daemon_log_tail(identity, 40);
    panic!(
        "assigned receiver never presented the stored film's ticket\n\
         --- last active.json ---\n{last}\n--- daemon log tail ---\n{log}"
    );
}

/// The last `lines` of every `daemon.log` under `root`.
fn daemon_log_tail(root: &Path, lines: usize) -> String {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.file_name().is_some_and(|name| name == "daemon.log") {
                out.push(path);
            }
        }
    }
    let mut logs = Vec::new();
    walk(root, &mut logs);
    if logs.is_empty() {
        return format!("<no daemon.log under {}>", root.display());
    }
    logs.iter()
        .map(|log| {
            let text = std::fs::read_to_string(log).unwrap_or_default();
            let tail: Vec<&str> = text.lines().rev().take(lines).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            format!("{}:\n{}", log.display(), tail.join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Upload a real container and author the film program that plays it.
///
/// The container is the demuxer's own fixture — the same bytes its unit tests
/// read — so ingest, plan, serve and this end-to-end are pinned to one shape.
async fn seed_stored_film(client: &Client, home: &Path, space: &str) -> String {
    let space_id = mechanics::ids::SpaceId::parse(space).expect("founded Space id");
    let store = registered_store(client, space).await;
    let film = mediabox::testkit::whole_file();
    let route = lait::control::ControlRoute::Orbit {
        address: lait::control::OrbitAddress::for_store(std::path::Path::new(&store), space_id),
    };
    let upload = lait::control::ContentUpload::open(
        home,
        route,
        [0xC1; 16],
        None,
        u64::try_from(film.len()).expect("film length"),
    )
    .await;
    let mut upload = match upload {
        Ok(upload) => upload,
        Err(error) => {
            let selection = lait::config::Selection::for_identity(home);
            let (daemon_home, pid, probe) = match lait::daemon::Client::for_selection(&selection) {
                Ok(daemon) => (
                    daemon.home().display().to_string(),
                    lait::config::daemon_pid(daemon.home()),
                    format!("{:?}", daemon.probe().await),
                ),
                Err(resolve) => (
                    "<unresolved>".to_owned(),
                    None,
                    format!("could not resolve daemon client: {resolve:#}"),
                ),
            };
            let log = daemon_log_tail(home, 80);
            panic!(
                "open the film upload: {error:#}\n\
                 daemon home: {daemon_home}\n\
                 recorded pid: {pid:?}\n\
                 probe after failure: {probe}\n\
                 --- daemon log tail ---\n{log}"
            );
        }
    };
    upload.push(&film).await.expect("push the film");
    let reply = upload.finish().await.expect("seal the film");
    let lait::control::ContentReply::ContentWritten {
        content,
        plaintext_len,
    } = reply
    else {
        panic!("the film upload was refused: {reply:?}");
    };

    let entry = signage::SignageMedia {
        id: replica::body::BodyId::from_bytes([0xC2; 16]).render(),
        name: "Premiere".into(),
        source: signage::contract::MediaSource::Stored {
            content,
            size: plaintext_len,
            mime: "video/mp4".into(),
        },
        duration_ms: None,
        width: None,
        height: None,
        catalog: None,
    };
    let film_program = signage::SignageProgram {
        id: replica::body::BodyId::from_bytes([0xC3; 16]).render(),
        name: "Premiere night".into(),
        cycle: signage::ProgramCycle::HoldLast,
        items: vec![signage::SignageItem {
            id: "film".into(),
            media: entry.id.clone(),
            duration_ms: Some(10_000),
        }],
        windows: Vec::new(),
    };
    let program_id = film_program.id.clone();
    let entry_id = entry.id.clone();
    write_signage_media(client, space, entry).await;
    write_signage_program(client, space, film_program).await;

    // The seed proves its own write: an entry the World cannot read back is a
    // program that will blank as "unsupported" twenty seconds from now, in a
    // place with no words for why.
    let space_id = mechanics::ids::SpaceId::parse(space).expect("founded Space id");
    let call = signage_app::encode_call(&signage_app::SignageRequest::MediaGet {
        media: entry_id.clone(),
    })
    .expect("encode Signage media read-back");
    let reply = client
        .daemon()
        .expect("identity daemon for Signage read-back")
        .call_world(
            lait::control::ControlRoute::World {
                address: lait::control::OrbitAddress::for_store(
                    std::path::Path::new(&store),
                    space_id,
                ),
                world: signage::contract::PRODUCT_WORLD.into(),
            },
            call.clone(),
            None,
        )
        .await
        .expect("read the Signage library entry back");
    let decoded = signage_app::decode_reply(&call, reply).expect("decode Signage media read-back");
    let response: signage_app::SignageResponse =
        serde_json::from_value(decoded).expect("typed Signage media read-back");
    let signage_app::SignageResponse::Media { media: read_back } = response else {
        panic!("the Signage World did not answer the media read-back: {response:?}");
    };
    assert!(
        read_back.as_ref().is_some_and(|row| row.id == entry_id),
        "the stored film's library entry did not read back after MediaSaved: {read_back:?}"
    );
    program_id
}

async fn seed_signage_program(client: &Client, store: &Path) -> (String, String) {
    client
        .space_found(
            &store.to_string_lossy(),
            "Display recovery",
            Some("Astrolabe".into()),
        )
        .await
        .expect("found a real Space for the Signage program");
    let context = client
        .host_context()
        .await
        .expect("read the founded Signage Orbit");
    let canonical_store = store.canonicalize().expect("canonical Signage store");
    let orbit = context
        .orbits
        .iter()
        .find(|orbit| {
            Path::new(&orbit.path)
                .canonicalize()
                .is_ok_and(|path| path == canonical_store)
        })
        .expect("founded Signage Orbit is registered");
    let welcome = signage_card(
        10,
        "Astrolabe is coordinating this display",
        "This frame came from the durable Signage World.",
        "102030",
    );
    let coordinated = signage_card(
        11,
        "Receivers share this program boundary",
        "Astrolabe supplied one group-aligned cursor.",
        "305010",
    );
    let program = signage::SignageProgram {
        id: replica::body::BodyId::from_bytes([9; 16]).render(),
        name: "Restart proof".into(),
        cycle: signage::ProgramCycle::Loop,
        items: vec![
            signage::SignageItem {
                id: "welcome".into(),
                media: welcome.id.clone(),
                duration_ms: Some(2_000),
            },
            signage::SignageItem {
                id: "coordinated".into(),
                media: coordinated.id.clone(),
                duration_ms: Some(2_000),
            },
        ],
        windows: Vec::new(),
    };
    for entry in [&welcome, &coordinated] {
        write_signage_media(client, &orbit.space, entry.clone()).await;
    }
    write_signage_program(client, &orbit.space, program.clone()).await;
    (orbit.space.clone(), program.id)
}

/// The store path as the daemon spelled it when it registered the Orbit.
///
/// The same resolution `client/display.rs` performs for a real caller: ask the
/// host for its Orbits and take the registered path, rather than assuming this
/// process and the daemon spell one directory the same way.
async fn registered_store(client: &Client, space: &str) -> String {
    let context = client
        .host_context()
        .await
        .expect("read the registered Orbits");
    context
        .orbits
        .iter()
        .find(|orbit| orbit.space == space)
        .map(|orbit| orbit.path.clone())
        .expect("the Space has a registered local Orbit")
}

/// A library entry holding one authored card.
///
/// Items name library entries rather than carrying content, so a program that
/// draws anything needs the library written first.
fn signage_card(tag: u8, title: &str, body: &str, background: &str) -> signage::SignageMedia {
    signage::SignageMedia {
        id: replica::body::BodyId::from_bytes([tag; 16]).render(),
        name: title.into(),
        source: signage::contract::MediaSource::Card {
            title: title.into(),
            body: body.into(),
            background: background.into(),
            foreground: "ffffff".into(),
        },
        duration_ms: Some(2_000),
        width: None,
        height: None,
        catalog: None,
    }
}

/// Put one library entry through the same real World adapter.
async fn write_signage_media(client: &Client, space: &str, media: signage::SignageMedia) {
    let space_id = mechanics::ids::SpaceId::parse(space).expect("founded Space id");
    let store = registered_store(client, space).await;
    let call = signage_app::encode_call(&signage_app::SignageRequest::MediaPut {
        media: media.clone(),
    })
    .expect("encode Signage media write");
    let reply = client
        .daemon()
        .expect("identity daemon for Signage write")
        .call_world(
            lait::control::ControlRoute::World {
                address: lait::control::OrbitAddress::for_store(
                    std::path::Path::new(&store),
                    space_id,
                ),
                world: signage::contract::PRODUCT_WORLD.into(),
            },
            call.clone(),
            None,
        )
        .await
        .expect("write the Signage library entry through its real World adapter");
    let decoded = signage_app::decode_reply(&call, reply).expect("decode Signage media reply");
    let response: signage_app::SignageResponse =
        serde_json::from_value(decoded).expect("typed Signage media reply");
    assert!(
        matches!(response, signage_app::SignageResponse::MediaSaved { media: ref saved } if saved == &media.id),
        "Signage World did not save the library entry: {response:?}"
    );
}

async fn write_signage_program(client: &Client, space: &str, program: signage::SignageProgram) {
    let space_id = mechanics::ids::SpaceId::parse(space).expect("founded Space id");
    // Address the Orbit by the path the *daemon* registered, never by the one
    // this test happens to hold. The Orbit id is derived from the path as
    // spelled — `normalize` settles separators, trailing slashes and Windows
    // case and deliberately resolves nothing — so two spellings of one
    // directory are two Orbits, and the host answers `InvalidCall`. A tempdir
    // reaches the daemon canonicalised, and neither spelling is recoverable
    // from the other: macOS adds `/private`, and Windows `canonicalize`
    // returns a `\\?\` UNC path the daemon never used. Production has this
    // right (`client/display.rs` resolves through `host_context`); the test
    // was the half that guessed, which is why it passed only where tempdirs
    // are already canonical.
    let store = registered_store(client, space).await;
    let store = std::path::Path::new(&store);
    let space = space_id;
    let call = signage_app::encode_call(&signage_app::SignageRequest::ProgramPut {
        program: program.clone(),
    })
    .expect("encode Signage program write");
    let reply = client
        .daemon()
        .expect("identity daemon for Signage write")
        .call_world(
            lait::control::ControlRoute::World {
                address: lait::control::OrbitAddress::for_store(store, space),
                world: signage::contract::PRODUCT_WORLD.into(),
            },
            call.clone(),
            None,
        )
        .await
        .expect("write the Signage program through its real World adapter");
    let decoded = signage_app::decode_reply(&call, reply).expect("decode Signage write reply");
    let response: signage_app::SignageResponse =
        serde_json::from_value(decoded).expect("typed Signage write reply");
    assert!(
        matches!(response, signage_app::SignageResponse::Saved { program: ref saved } if saved == &program.id),
        "Signage World did not save the receiver program: {response:?}"
    );
}

async fn schedule_signage_boundary(client: &Client, space: &str, program: &str) -> u64 {
    let now = mechanics::wallclock::now_millis();
    let boundary = now
        .checked_add(10_999)
        .map(|value| value / 1_000 * 1_000)
        .expect("schedule boundary within the test clock");
    let start = boundary.saturating_sub(60_000);
    let local = |unix_ms: u64| {
        jiff::Timestamp::from_millisecond(i64::try_from(unix_ms).expect("test time fits i64"))
            .expect("valid test timestamp")
            .to_zoned(jiff::tz::TimeZone::UTC)
            .datetime()
            .to_string()
    };
    let before = signage_card(
        12,
        "Before the schedule boundary",
        "The coordinator is holding an exact wake deadline.",
        "102030",
    );
    let after = signage_card(
        13,
        "After the schedule boundary",
        "This revision arrived without another World write.",
        "305010",
    );
    let scheduled = signage::SignageProgram {
        id: program.to_owned(),
        name: "Boundary proof".into(),
        cycle: signage::ProgramCycle::HoldLast,
        items: vec![
            signage::SignageItem {
                id: "before-boundary".into(),
                media: before.id.clone(),
                duration_ms: Some(60_000),
            },
            signage::SignageItem {
                id: "after-boundary".into(),
                media: after.id.clone(),
                duration_ms: Some(60_000),
            },
        ],
        windows: vec![
            signage::SignageWindow {
                id: "before".into(),
                window: schedule::Window {
                    start_local: local(start),
                    duration_ms: boundary.saturating_sub(start),
                    recurrence: schedule::Recurrence::None,
                    until_unix_ms: None,
                    priority: 0,
                    enabled: true,
                    timezone: "UTC".into(),
                    exceptions: Vec::new(),
                },
                items: vec!["before-boundary".into()],
            },
            signage::SignageWindow {
                id: "after".into(),
                window: schedule::Window {
                    start_local: local(boundary),
                    duration_ms: 60_000,
                    recurrence: schedule::Recurrence::None,
                    until_unix_ms: None,
                    priority: 0,
                    enabled: true,
                    timezone: "UTC".into(),
                    exceptions: Vec::new(),
                },
                items: vec!["after-boundary".into()],
            },
        ],
    };
    assert!(scheduled.validate(), "scheduled Signage program is valid");
    for entry in [&before, &after] {
        write_signage_media(client, space, entry.clone()).await;
    }
    write_signage_program(client, space, scheduled).await;
    boundary
}

async fn wait_for_receiver_exit(receiver: &mut OwnedReceiver) {
    for _ in 0..150 {
        if let Some(status) = receiver.0.try_wait().expect("inspect reference receiver") {
            assert!(
                status.success(),
                "reference receiver exited unsuccessfully: {status}"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("revoked reference receiver did not leave its live loop");
}

/// Stop the daemon the head started under itself.
///
/// The head spawns it; this supervisor never owned it, so nothing here may
/// force-kill it. Asking it to stop is the only move available, and it is the
/// right one — the same request `reload` uses.
async fn stop_daemon(home: &Path) {
    let selection = lait::config::Selection::for_identity(home);
    let Ok(daemon) = lait::daemon::Client::for_selection(&selection) else {
        return;
    };
    let _ = daemon
        .request(
            lait::control::ControlRoute::Daemon,
            &lait::control::Request::Stop,
            None,
        )
        .await;
}

/// The whole handoff, minus the browser.
///
/// One test rather than four, because the value is in the chain: a head that
/// comes up but mints nothing, and a ticket that mints but never expires, are
/// both failures of `Open` rather than of a component.
#[tokio::test(flavor = "multi_thread")]
async fn a_head_comes_up_and_mints_a_credential_worth_exactly_one_use() {
    let Some(executable) = sidecar() else {
        // A failure, not a skip. This is the one test that exercises the
        // client-to-process seam against a real binary, and that seam has been
        // wrong twice — both times with every component correct and the
        // composition wrong. A run that cannot find `lait` has proven nothing,
        // and reporting `ok` for it is how the guard comes to be trusted while
        // guarding nothing.
        panic!(
            "no lait binary beside the test binary, so the launch seam was not exercised.              Build it first: `cargo build -p lait`, or run the suite that does              (`cargo nextest run --workspace`)."
        );
    };

    // This test drives a real receiver through pairing, so it needs the display
    // coordinator — and the coordinator binds a fixed `0.0.0.0:7443`, which makes
    // it a machine-wide singleton. A daemon that loses that race now degrades to
    // serving without display coordination rather than refusing to start, which
    // is right for the product and leaves this test with nothing to pair against.
    //
    // Said here, before ten seconds of polling. Without it the failure is
    // "reference receiver did not open a pairing within ten seconds" — a symptom
    // that names neither the port nor the process holding it, which is the class
    // of message this suite exists to stop shipping.
    if let Err(error) = std::net::TcpListener::bind((
        std::net::Ipv4Addr::UNSPECIFIED,
        lait::display::DEFAULT_DISPLAY_PORT,
    )) {
        panic!(
            "this test needs the display coordinator, which binds 0.0.0.0:{port}, and \
             something else on this machine holds it ({error}). A running `lait` daemon is \
             the usual holder — stop it, or run this test where none is running. Setting \
             LAIT_DISPLAY=off does not help: it removes the coordinator this test drives.",
            port = lait::display::DEFAULT_DISPLAY_PORT,
        );
    }

    let managed = tempfile::tempdir().expect("a managed root");
    let identity = tempfile::tempdir().expect("an identity home");
    let bundled_worlds = tempfile::tempdir().expect("bundled first-party World releases");
    stage_bundled_worlds(bundled_worlds.path());
    // Declared before the client, so it drops after it: the daemon is asked to
    // stop once nothing is still speaking to it, and before the temporary homes
    // it is holding open are removed.
    let _daemon_stopped = DaemonStopped(identity.path().to_path_buf());

    let mut config = Config::new(managed.path().to_path_buf(), executable.clone());
    config.identity = Some(identity.path().to_path_buf());
    config.bundled_worlds = Some(bundled_worlds.path().to_path_buf());
    let (client, signals) = Client::start(config)
        .await
        .expect("a client that starts its identity daemon");

    let selection = lait::config::Selection::for_identity(identity.path());
    let daemon = lait::daemon::Client::for_selection(&selection).expect("the identity daemon");
    assert!(
        matches!(daemon.probe().await, lait::control::Probe::Healthy { .. }),
        "client startup returned before its identity daemon answered"
    );
    let displays = client
        .display_status()
        .await
        .expect("the daemon-owned display coordinator answers Astrolabe");
    assert!(
        displays.origin.starts_with("https://")
            && !displays.certificate_sha256.is_empty()
            && displays
                .certificate_pem
                .starts_with("-----BEGIN CERTIFICATE-----\n"),
        "the display coordinator announced no pinned HTTPS identity"
    );
    assert!(
        displays.surfaces.iter().any(|surface| {
            surface.world == "com.lait.signage" && surface.surface == "signage.program"
        }),
        "the process serving Astrolabe omitted the bundled Signage display surface"
    );
    let first_pid = lait::config::daemon_pid(daemon.home()).expect("the started daemon's pid");

    // A second client is the existing-daemon half of the startup contract. It
    // attaches to the process already serving this identity rather than racing
    // it with another sidecar spawn.
    let second_managed = tempfile::tempdir().expect("another managed root");
    let mut second_config = Config::new(second_managed.path().to_path_buf(), executable.clone());
    second_config.identity = Some(identity.path().to_path_buf());
    let (attached, _attached_signals) = Client::start(second_config)
        .await
        .expect("a second client that attaches to the running identity daemon");
    assert_eq!(
        lait::config::daemon_pid(daemon.home()),
        Some(first_pid),
        "attaching to a running identity started a competing daemon"
    );
    let attached_displays = attached
        .display_status()
        .await
        .expect("the attached client reaches the same display coordinator");
    assert_eq!(
        attached_displays.instance, displays.instance,
        "attaching to the daemon produced a second display coordinator"
    );
    assert_eq!(
        attached_displays.certificate_sha256, displays.certificate_sha256,
        "the attached client observed another display trust identity"
    );
    attached.shutdown().await;

    let (signage_orbit, signage_program) = seed_signage_program(&client, identity.path()).await;

    // Run the real receiver binary against the real coordinator. This is the
    // restart/recovery seam: the public certificate copied by Astrolabe must
    // establish TLS, both halves of the pairing ceremony must enroll one
    // durable device, and the same protected receiver credential must be able
    // to authenticate to a fresh daemon process before revocation can stop it.
    let (client, signals) = if let Some(receiver_executable) = reference_receiver() {
        let receiver_root = tempfile::tempdir().expect("reference receiver root");
        let bootstrap_path = receiver_root.path().join("bootstrap.json");
        let state_path = receiver_root.path().join("state");
        let output_path = receiver_root.path().join("output");
        let bootstrap = serde_json::json!({
            "protocol_major": 1,
            "trust": {
                "kind": "pinned_certificate",
                "origin": displays.origin.clone(),
                "sha256": displays.certificate_sha256.clone(),
            },
            "certificate_pem": displays.certificate_pem.clone(),
            "rendezvous": null,
        });
        std::fs::write(
            &bootstrap_path,
            serde_json::to_vec_pretty(&bootstrap).expect("encode receiver bootstrap"),
        )
        .expect("write receiver bootstrap");
        let child = Command::new(&receiver_executable)
            .arg("--bootstrap")
            .arg(&bootstrap_path)
            .arg("--state")
            .arg(&state_path)
            .arg("--output")
            .arg(&output_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("launch the reference display receiver");
        let mut receiver = OwnedReceiver(child);
        let pairing = wait_for_pairing(&client).await;
        receiver
            .0
            .stdin
            .take()
            .expect("receiver confirmation input")
            .write_all(b"yes\n")
            .expect("confirm pairing at the receiver");
        client
            .display_pairing_approve(pairing, "Restart receiver".to_owned())
            .await
            .expect("approve the receiver in Astrolabe");
        let device = wait_for_receiver(&client).await;
        wait_for_unassigned(&output_path.join("active.json"), &device).await;
        let sync_group = "lobby-wall";
        client
            .display_assignment_put(DisplayAssignmentInput {
                device: device.clone(),
                // The Astrolabe surface selects by Space. The client boundary
                // resolves this to the exact local Orbit id before it reaches
                // the daemon.
                orbit: signage_orbit,
                world: signage::contract::PRODUCT_WORLD.into(),
                surface: "signage.program".into(),
                input: serde_json::json!({ "program": signage_program }),
                theme: lait::control::DisplayThemeSetting::Dark,
                stale_after_ms: 60_000,
                on_stale: lait::control::DisplayStaleActionSetting::Blank,
                sync: Some(lait::control::DisplayAssignmentSyncSetting {
                    group: sync_group.into(),
                    mode: lait::control::DisplaySyncModeSetting::Positional,
                    static_delay_ms: 0,
                }),
                expires_at_unix_ms: None,
            })
            .await
            .expect("assign the durable Signage program in Astrolabe");
        let assigned_status = client
            .display_status()
            .await
            .expect("read committed display assignment");
        let assignment = assigned_status
            .assignments
            .iter()
            .find(|row| row.device == device && row.revoked_at_unix_ms.is_none())
            .expect("the receiver has one active assignment");
        let assignment_id = assignment.assignment.clone();
        let receiver_program = assignment.program.clone();
        let revision = wait_for_assigned(
            &output_path.join("active.json"),
            &assignment_id,
            &receiver_program,
            identity.path(),
        )
        .await;
        let frame = std::fs::read(output_path.join("frame.png"))
            .expect("read the atomically presented Signage frame");
        assert_eq!(
            frame.get(..8),
            Some(b"\x89PNG\r\n\x1a\n".as_slice()),
            "the assigned Signage surface did not present a PNG frame"
        );
        wait_for_health(&client, &device, &revision).await;

        // A second independently paired process joins the same requested
        // positional group. Neither reference receiver declares positional
        // sync — the native-HLS profile declares none at all — so the
        // coordinator must degrade the whole group to one shared boundary
        // cursor rather than pretend positional guarantees exist.
        let second_receiver_root = tempfile::tempdir().expect("second reference receiver root");
        let second_bootstrap_path = second_receiver_root.path().join("bootstrap.json");
        let second_state_path = second_receiver_root.path().join("state");
        let second_output_path = second_receiver_root.path().join("output");
        std::fs::write(
            &second_bootstrap_path,
            serde_json::to_vec_pretty(&bootstrap).expect("encode second receiver bootstrap"),
        )
        .expect("write second receiver bootstrap");
        let second_child = Command::new(&receiver_executable)
            .arg("--bootstrap")
            .arg(&second_bootstrap_path)
            .arg("--state")
            .arg(&second_state_path)
            .arg("--output")
            .arg(&second_output_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("launch the second reference display receiver");
        let mut second_receiver = OwnedReceiver(second_child);
        let second_pairing = wait_for_pairing(&client).await;
        second_receiver
            .0
            .stdin
            .take()
            .expect("second receiver confirmation input")
            .write_all(b"yes\n")
            .expect("confirm pairing at the second receiver");
        client
            .display_pairing_approve(second_pairing, "Synced receiver".to_owned())
            .await
            .expect("approve the second receiver in Astrolabe");
        let second_device = wait_for_new_receiver(&client, &device).await;
        wait_for_unassigned(&second_output_path.join("active.json"), &second_device).await;
        client
            .display_assignment_put(DisplayAssignmentInput {
                device: second_device.clone(),
                orbit: assignment.space.clone(),
                world: signage::contract::PRODUCT_WORLD.into(),
                surface: "signage.program".into(),
                input: serde_json::json!({ "program": signage_program }),
                theme: lait::control::DisplayThemeSetting::Dark,
                stale_after_ms: 60_000,
                on_stale: lait::control::DisplayStaleActionSetting::Blank,
                sync: Some(lait::control::DisplayAssignmentSyncSetting {
                    group: sync_group.into(),
                    mode: lait::control::DisplaySyncModeSetting::Positional,
                    static_delay_ms: 0,
                }),
                expires_at_unix_ms: None,
            })
            .await
            .expect("assign the second receiver to the sync group");
        let synced_status = client
            .display_status()
            .await
            .expect("read the second committed display assignment");
        let second_assignment = synced_status
            .assignments
            .iter()
            .find(|row| row.device == second_device && row.revoked_at_unix_ms.is_none())
            .expect("the second receiver has one active assignment");
        assert_eq!(
            second_assignment
                .sync
                .as_ref()
                .map(|sync| sync.group.as_str()),
            Some(sync_group),
            "the second assignment lost its sync group"
        );
        let second_assignment_id = second_assignment.assignment.clone();
        let second_receiver_program = second_assignment.program.clone();
        let second_revision = wait_for_assigned(
            &second_output_path.join("active.json"),
            &second_assignment_id,
            &second_receiver_program,
            identity.path(),
        )
        .await;
        wait_for_health(&client, &second_device, &second_revision).await;
        wait_for_group_boundary(
            &output_path.join("active.json"),
            &second_output_path.join("active.json"),
            sync_group,
        )
        .await;

        // One World write installs two civil-time windows. The invalidation
        // pushes the first semantic revision; after that, no actor or World
        // mutation occurs. The package's exact boundary deadline completes the
        // held long poll, recompiles, and pushes the second semantic revision.
        let boundary =
            schedule_signage_boundary(&client, &assignment.space, &signage_program).await;
        let before_revision = wait_for_revision_change(
            &output_path.join("active.json"),
            &assignment_id,
            &receiver_program,
            &revision,
            "pre-boundary",
        )
        .await;
        let second_before_revision = wait_for_revision_change(
            &second_output_path.join("active.json"),
            &second_assignment_id,
            &second_receiver_program,
            &second_revision,
            "second pre-boundary",
        )
        .await;
        let before_frame =
            std::fs::read(output_path.join("frame.png")).expect("read pre-boundary frame");
        assert!(
            mechanics::wallclock::now_millis() < boundary,
            "the test failed to observe the pre-boundary revision before its deadline"
        );
        let revision = wait_for_revision_change(
            &output_path.join("active.json"),
            &assignment_id,
            &receiver_program,
            &before_revision,
            "post-boundary",
        )
        .await;
        let second_revision = wait_for_revision_change(
            &second_output_path.join("active.json"),
            &second_assignment_id,
            &second_receiver_program,
            &second_before_revision,
            "second post-boundary",
        )
        .await;
        assert!(
            mechanics::wallclock::now_millis() >= boundary,
            "the semantic revision changed before the schedule boundary"
        );
        let after_frame =
            std::fs::read(output_path.join("frame.png")).expect("read post-boundary frame");
        assert_ne!(
            before_frame, after_frame,
            "the boundary revision did not change presented content"
        );
        wait_for_health(&client, &device, &revision).await;
        wait_for_health(&client, &second_device, &second_revision).await;

        // A stored film, end to end: a real container uploaded to the content
        // plane, a Stored library entry, a program that plays it, and the
        // receiver presenting a ticketed playlist URL. The ticket is minted
        // only after the coordinator walks the container's own bytes and
        // installs the planned presentation, so this one assertion covers the
        // whole serve-side chain.
        let film_program = seed_stored_film(&client, identity.path(), &assignment.space).await;
        client
            .display_assignment_put(DisplayAssignmentInput {
                device: device.clone(),
                orbit: assignment.space.clone(),
                world: signage::contract::PRODUCT_WORLD.into(),
                surface: "signage.program".into(),
                input: serde_json::json!({ "program": film_program }),
                theme: lait::control::DisplayThemeSetting::Dark,
                stale_after_ms: 60_000,
                on_stale: lait::control::DisplayStaleActionSetting::Blank,
                sync: None,
                expires_at_unix_ms: None,
            })
            .await
            .expect("assign the stored film in Astrolabe");
        let film_status = client
            .display_status()
            .await
            .expect("read the committed film assignment");
        let film_assignment = film_status
            .assignments
            .iter()
            .find(|row| row.device == device && row.revoked_at_unix_ms.is_none())
            .expect("the receiver has one active film assignment");
        let assignment_id = film_assignment.assignment.clone();
        let receiver_program = film_assignment.program.clone();
        let (film_revision, film_url) = wait_for_media(
            &output_path.join("active.json"),
            &assignment_id,
            &receiver_program,
            identity.path(),
        )
        .await;
        assert!(
            film_url.starts_with(&displays.origin),
            "the handoff URL {film_url} left the pinned origin {}",
            displays.origin
        );
        assert!(
            film_url.contains("/head/v1/live/"),
            "the handoff URL {film_url} is not a ticketed playlist"
        );
        assert!(
            film_url.ends_with("/master.m3u8"),
            "the handoff URL {film_url} is not an HLS master playlist"
        );
        wait_for_health_staged(&client, &device, &film_revision, 0..=0).await;

        client.shutdown().await;
        drop(signals);
        drop(client);
        stop_daemon(identity.path()).await;
        wait_for_daemon_stop(identity.path()).await;
        let mut restarted_config = Config::new(managed.path().to_path_buf(), executable.clone());
        restarted_config.identity = Some(identity.path().to_path_buf());
        let (restarted, restarted_signals) = Client::start(restarted_config)
            .await
            .expect("Astrolabe restarts the identity daemon");
        let restarted_displays = restarted
            .display_status()
            .await
            .expect("restarted daemon restores display state");
        assert_eq!(restarted_displays.instance, displays.instance);
        assert_eq!(
            restarted_displays.certificate_sha256,
            displays.certificate_sha256
        );
        assert!(
            restarted_displays
                .devices
                .iter()
                .any(|row| row.device == device),
            "restarted daemon lost its enrolled display"
        );
        assert!(
            restarted_displays.assignments.iter().any(|row| {
                row.assignment == assignment_id
                    && row.program == receiver_program
                    && row.revoked_at_unix_ms.is_none()
            }),
            "restarted daemon lost its active Signage assignment"
        );
        assert!(
            restarted_displays.assignments.iter().any(|row| {
                row.assignment == second_assignment_id
                    && row.program == second_receiver_program
                    && row.revoked_at_unix_ms.is_none()
                    && row
                        .sync
                        .as_ref()
                        .is_some_and(|sync| sync.group == sync_group)
            }),
            "restarted daemon lost the second synchronized Signage assignment"
        );
        // The first device carries the film across the restart; its health
        // reports the film revision and stages a grant, not bytes.
        wait_for_health_staged(&restarted, &device, &film_revision, 0..=0).await;
        wait_for_health(&restarted, &second_device, &second_revision).await;
        restarted
            .display_device_revoke(device.clone())
            .await
            .expect("revoke the recovered receiver");
        restarted
            .display_device_revoke(second_device)
            .await
            .expect("revoke the recovered synchronized receiver");
        wait_for_receiver_exit(&mut receiver).await;
        wait_for_receiver_exit(&mut second_receiver).await;
        (restarted, restarted_signals)
    } else {
        eprintln!(
            "no astrolabe-display-reference binary beside the test binary; skipping the real receiver restart seam"
        );
        (client, signals)
    };

    let head = client
        .head("issues")
        .await
        .expect("a head for this identity");
    assert!(
        head.base.starts_with("http://127.0.0.1:"),
        "a head came up somewhere other than loopback: {}",
        head.base
    );
    assert!(!head.token.is_empty(), "a head announced no credential");

    // The head has to serve the identity the Library was read from. This is the
    // assertion the defect above walked straight past: a head started at the
    // daemon's own directory comes up, announces an address and mints a ticket
    // — and serves a self-contained identity nobody has ever used, so `Open`
    // lands on a head with no Spaces in it.
    let orientation = post_json(
        &head,
        "/api/host/rpc",
        &serde_json::json!({ "cmd": "host_context" }),
    )
    .await
    .expect("the head answers for an identity");
    let serving = orientation["identity_home"]
        .as_str()
        .expect("a head that says which identity it serves");
    let expected = client.identity().expect("a bound identity");
    assert_eq!(
        Path::new(serving).canonicalize().ok(),
        expected.canonicalize().ok(),
        "the head serves {serving}, and this client is bound to {}",
        expected.display()
    );

    // Asking twice finds the head that is already up. The alternative is a port
    // and a run credential per click.
    let again = client.head("issues").await.expect("the same head");
    assert_eq!(again, head, "a second Open started a second head");

    // A different World is a different head, which is the whole point: one
    // process per World is what makes stopping one a statement about that
    // World rather than about whatever else shared it.
    let signage = client
        .head("signage")
        .await
        .expect("a head for the other World");
    assert_ne!(
        signage.base, head.base,
        "two Worlds were served by one head"
    );

    // Two Worlds, two heads — and *exactly* two. The assertion this replaces
    // said `len() == 1`, "one identity acquired more than one head", written when
    // one head served everything. It was left standing directly under the block
    // above that starts a second head, so it could not pass; the display-pairing
    // wait earlier in this test failed first and hid it. Per-World heads reached
    // CI unproven against a real binary because of it.
    //
    // The count is what matters, not just that the bases differ: the defect this
    // is guarding is a *third* head, which is what `Option<&str>` produced when
    // one caller named a World and another did not.
    let heads = client.heads();
    assert_eq!(
        heads.len(),
        2,
        "two Worlds is two heads, and no more: {:?}",
        heads.iter().map(|h| (&h.world, &h.url)).collect::<Vec<_>>()
    );
    let mut served: Vec<Option<String>> = heads.iter().map(|h| h.world.clone()).collect();
    served.sort();
    assert_eq!(
        served,
        vec![Some("issues".to_owned()), Some("signage".to_owned())],
        "each head names the World it actually announced"
    );

    // Asking again for either World finds the head that is already up, rather
    // than spending a third port. This is the property the key exists for, and
    // it is checked *after* both are running because that is when a key
    // collision would show.
    assert_eq!(
        client.head("issues").await.expect("the issues head").base,
        head.base
    );
    assert_eq!(
        client.head("signage").await.expect("the signage head").base,
        signage.base
    );
    assert_eq!(client.heads().len(), 2, "asking again started nothing new");

    let minted = client.mint(&head).await.expect("a launch credential");
    assert!(!minted.secret.is_empty());
    assert!(
        minted.expires_at_ms
            > u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("a clock after the epoch")
                    .as_millis()
            )
            .unwrap_or(u64::MAX),
        "a launch credential was minted already expired"
    );

    let launch = Client::launch_url(&head.base, "/", &minted.secret, minted.expires_at_ms)
        .expect("a launch url");
    assert!(
        launch.url.contains(&minted.secret),
        "the composed URL carries no credential"
    );

    // Two tickets are two credentials. A head that answered the same secret
    // twice would make "single-use" a property of the first launch only.
    let second = client.mint(&head).await.expect("a second credential");
    assert_ne!(
        second.secret, minted.secret,
        "a head minted the same credential twice"
    );

    // And a request the credential does not authorise is refused. The launch
    // ticket is weaker than the run token by construction; this is the run
    // token being what `/api/launch` actually requires.
    let unauthorised = Head {
        base: head.base.clone(),
        token: "0".repeat(64),
    };
    let refused = post_json(
        &unauthorised,
        "/api/launch",
        &serde_json::json!({ "orbit": "orb_test" }),
    )
    .await
    .expect_err("a head minted a credential for an unauthenticated caller");
    assert!(
        !refused.retryable,
        "a refusal was reported as worth trying again: {refused}"
    );

    client.shutdown().await;
    drop(signals);
    stop_daemon(identity.path()).await;
}

// ---------------------------------------------------------------------------
// The staged-swap chain (CLIENT-65).
//
// The seam: a signed feed names a tree artifact, `lait::update::tree` stages
// it beside the live tree, and the `astrolabe-stub` launcher — a real
// process, spawned here — proves it and swaps it in by rename before
// starting the client. Every part is unit-tested where it lives; this is the
// composition, which is the thing this file exists to assert. The "client"
// the trees carry is `chain-probe`, a reference binary that announces the
// version of the tree it actually ran from, so the assertions below are
// about *which* tree launched, never merely that something did.

/// The stub binary beside the test binary, or a panic — like `sidecar()`,
/// and unlike the reference receiver: the stub is the thing under test, and
/// reporting `ok` without it would be a guard trusted while guarding
/// nothing.
fn stub_binary() -> PathBuf {
    built_binary("astrolabe-stub").unwrap_or_else(|| {
        panic!(
            "no astrolabe-stub binary beside the test binary, so the staged-swap seam was not \
             exercised; build the workspace bins (cargo build -p astrolabe-stub) first"
        )
    })
}

/// The name the stub is *installed* under: the application's own, at the
/// install root. The build name (`astrolabe-stub`) exists in no installation
/// — every installer renames it — and the daemon's
/// `lait::update::watch::install_root_of` keys on the installed name, so a
/// fixture using the build name would exercise a layout that never ships.
fn installed_stub_name() -> &'static str {
    if cfg!(windows) {
        "astrolabe.exe"
    } else {
        "astrolabe"
    }
}

/// The reference entry binary the fabricated trees carry.
fn probe_binary() -> PathBuf {
    built_binary("chain-probe").unwrap_or_else(|| {
        panic!(
            "no chain-probe binary beside the test binary, so the staged-swap seam was not \
             exercised; build the workspace bins (cargo build -p astrolabe-stub) first"
        )
    })
}

/// The tree's entry name on this platform — the same convention
/// `lait::update::tree` records and the stub launches.
fn tree_entry_name() -> &'static str {
    if cfg!(windows) {
        "astrolabe.exe"
    } else {
        "astrolabe"
    }
}

/// The sidecar name the pair contract requires beside the entry.
fn tree_sidecar_name() -> &'static str {
    if cfg!(windows) {
        "lait.exe"
    } else {
        "lait"
    }
}

/// A target triple whose platform half matches this host, so the staged
/// entry name and the launched entry name agree.
fn tree_target() -> &'static str {
    if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

/// Seal a payload into the feed's envelope: base64 payload bytes, detached
/// ed25519 over exactly those bytes. The shape `feed::open_envelope`
/// verifies; sealed here with `mechanics` directly because the feed's own
/// test sealer is crate-private — which is right, and this duplication is
/// itself covered by the resolve call below refusing anything misshapen.
fn seal_feed_object(payload: &serde_json::Value, seed: &[u8; 32]) -> Vec<u8> {
    let bytes = serde_json::to_vec(payload).expect("a payload encodes");
    let signature = mechanics::actor::sign_detached(seed, &bytes);
    serde_json::json!({
        "payload": data_encoding::BASE64.encode(&bytes),
        "signature": data_encoding::BASE64.encode(&signature),
    })
    .to_string()
    .into_bytes()
}

/// A tree artifact as the feed publishes them: gzip'd tar, one root
/// directory, entry executable at the root.
fn tree_artifact(version: &str, entry_bytes: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let root = format!("astrolabe-{version}");
        let mut file = |path: &str, contents: &[u8], mode: u32| {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("{root}/{path}"), contents)
                .expect("a tree entry appends");
        };
        file(tree_entry_name(), entry_bytes, 0o755);
        // The sidecar half of the pair. Not a real lait — the stager's
        // contract is that the tree carries one at its root, which is what
        // makes sidecar::beside and custody_of agree after a swap, and that
        // shape is what this fixture has to honour.
        file(tree_sidecar_name(), b"the sidecar half of the pair", 0o755);
        file("version.txt", version.as_bytes(), 0o644);
        file("data/asset.bin", b"an asset the tree carries", 0o644);
        builder
            .into_inner()
            .expect("the tar seals")
            .finish()
            .expect("the gzip seals");
    }
    bytes
}

/// Seal a whole feed — pointer and manifest — naming one tree artifact, and
/// resolve it with the matching key, exactly as an installed machine would.
fn resolve_sealed_tree_release(
    version: &str,
    archive: &[u8],
) -> (
    std::collections::HashMap<String, Vec<u8>>,
    lait::update::feed::Resolved,
) {
    let seed = [41u8; 32];
    let pubkey_hex = mechanics::actor::device_from_seed(&seed)
        .as_str()
        .to_string();
    let decoded = data_encoding::HEXLOWER
        .decode(pubkey_hex.as_bytes())
        .expect("a device id is lowercase hex of the public key");
    let pubkey: [u8; 32] = decoded.try_into().expect("a feed key is exactly 32 bytes");

    let url = format!("https://feed.example/releases/{version}/astrolabe-tree.tar.gz");
    let manifest = serde_json::json!({
        "version": version,
        "bundles": { lait::update::tree::TREE_BUNDLE: version },
        "artifacts": { lait::update::tree::TREE_BUNDLE: { tree_target(): {
            "url": url,
            "blake3": blake3_hex(archive),
            "size": archive.len(),
        }}},
    });
    let pointer = serde_json::json!({
        "kind": "release",
        "version": version,
        "manifest": "https://feed.example/releases/manifest.json",
    });

    let mut objects = std::collections::HashMap::new();
    objects.insert(
        "https://feed.example/channels/test".to_string(),
        seal_feed_object(&pointer, &seed),
    );
    objects.insert(
        "https://feed.example/releases/manifest.json".to_string(),
        seal_feed_object(&manifest, &seed),
    );
    objects.insert(url, archive.to_vec());

    let resolved = lait::update::feed::resolve_with(
        |asked| {
            objects.get(asked).cloned().ok_or_else(|| {
                lait::update::feed::Failure::Unreachable(format!("no object at {asked}"))
            })
        },
        lait::update::feed::Channel::Test,
        "https://feed.example",
        &[pubkey],
        None,
    )
    .expect("the sealed feed resolves against its own key");
    (objects, resolved)
}

/// blake3 as the feed manifests spell it: lowercase hex.
fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Run the stub as a real process against `root`, with the probe announcing
/// into `root`, and wait for it to exit.
///
/// The stub holds its claim for the lifetime of the client it starts, so
/// waiting on the stub waits on the whole tree of processes — which is also
/// what keeps the next phase from racing a still-exiting client over the
/// directory it is about to rename.
fn run_stub(root: &Path) {
    let status = Command::new(root.join(installed_stub_name()))
        .env("CHAIN_PROBE_ANNOUNCE", root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the stub spawns");
    assert!(
        status.success(),
        "the stub exited with a failure, and the stub must launch even when it refuses to apply"
    );
}

/// Wait for the probe's announcement and hand back the version it saw.
fn wait_for_announcement(root: &Path) -> String {
    let path = root.join("launched.txt");
    for _ in 0..150 {
        if let Ok(version) = std::fs::read_to_string(&path) {
            std::fs::remove_file(&path).expect("the announcement is consumed");
            return version;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("no tree announced itself within the budget");
}

/// The chain, end to end: signed pointer → manifest → verified tree
/// artifact → staged tree → a real stub process swaps by rename → the
/// launched entry announces the *new* tree — with the deferred, tampered,
/// and rollback arms asserted on the same install root.
#[test]
fn a_staged_release_is_applied_by_the_stub_and_the_previous_tree_survives() {
    let stub = stub_binary();
    let probe = probe_binary();
    let probe_bytes = std::fs::read(&probe).expect("the probe binary's bytes");

    let scratch = tempfile::tempdir().expect("an install root");
    let root = scratch.path();
    // Installed under the application's name, exactly as every installer
    // lays it down — the build name `astrolabe-stub` exists in no
    // installation, and `lait::update::watch::install_root_of` keys on the
    // installed name to decide whether staging happens at all.
    std::fs::copy(&stub, root.join(installed_stub_name()))
        .expect("the stub lands in the install root");

    // The live tree, version 0.0.1 — the install as the person has it.
    let current = root.join("current");
    std::fs::create_dir(&current).expect("the live tree");
    std::fs::copy(&probe, current.join(tree_entry_name())).expect("the live entry");
    std::fs::write(current.join("version.txt"), "0.0.1").expect("the live version");

    // Release 0.0.2, sealed into a feed and staged exactly as a daemon
    // would: resolve against the pinned key, verify, extract, record.
    let archive = tree_artifact("0.0.2", &probe_bytes);
    let (objects, resolved) = resolve_sealed_tree_release("0.0.2", &archive);
    let staged = lait::update::tree::stage_tree_with(
        |asked, _limit| {
            objects.get(asked).cloned().ok_or_else(|| {
                lait::update::feed::Failure::Unreachable(format!("no object at {asked}"))
            })
        },
        &resolved,
        tree_target(),
        root,
    )
    .expect("the 0.0.2 tree stages");
    assert_eq!(
        staged.version, "0.0.2",
        "the stage carries the release version"
    );

    // Recorded before anything moves, so the path-stability assertion after
    // the swap compares against what the person actually had.
    let entry_before_swap = current.join(tree_entry_name());

    // A live client holds the installation: the apply defers, is said, and
    // the person still gets their client — the old one.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("instance.lock"))
        .expect("the instance lock file");
    fs2::FileExt::try_lock_exclusive(&lock).expect("the test plays the live client");
    run_stub(root);
    assert_eq!(
        wait_for_announcement(root).trim(),
        "0.0.1",
        "an apply ran under a live client, or the wrong tree launched"
    );
    assert!(
        root.join("staged.manifest.json").is_file(),
        "a deferred stage was consumed"
    );
    fs2::FileExt::unlock(&lock).expect("the live client exits");

    // The lock is free: this launch applies, and the new tree is what runs.
    run_stub(root);
    assert_eq!(
        wait_for_announcement(root).trim(),
        "0.0.2",
        "the staged release did not become the running client"
    );
    assert_eq!(
        std::fs::read_to_string(current.join("version.txt")).expect("the live version"),
        "0.0.2",
        "the live tree is not the staged release"
    );
    assert!(
        !root.join("staged.manifest.json").exists(),
        "a consumed stage manifest was left behind"
    );
    // The pair, after the swap: astrolabe and lait as flat siblings, which
    // is what sidecar::beside and custody_of both mean by "beside". A swap
    // that delivered a tree without it would install a client that cannot
    // find its daemon.
    assert!(
        current.join(tree_sidecar_name()).is_file(),
        "the swapped-in tree does not carry its sidecar beside the entry"
    );
    // The entry's path is the same string it was before the update. This is
    // the half of the macOS identity rule a test can hold: TCC grants key on
    // signing identity, bundle id and *path*, so a layout that versioned the
    // live directory — Squirrel's `app-1.0.0/`, the obvious alternative to
    // this one — would silently drop every permission the person had granted
    // on the first update. The stable `current/` name is what prevents it,
    // and an assertion is what keeps it stable.
    assert_eq!(
        current.join(tree_entry_name()),
        entry_before_swap,
        "the update moved the client's path, which is how macOS loses TCC grants"
    );

    // The previous tree is kept, and kept *bootable*: it runs and announces
    // itself, which is what makes it a rollback target rather than a copy.
    let previous_entry = root.join("previous").join(tree_entry_name());
    let status = Command::new(&previous_entry)
        .env("CHAIN_PROBE_ANNOUNCE", root)
        .status()
        .expect("the previous tree's entry spawns");
    assert!(status.success(), "the previous tree is not bootable");
    assert_eq!(
        wait_for_announcement(root).trim(),
        "0.0.1",
        "the kept previous tree is not the prior release"
    );

    // A tampered stage: release 0.0.3 stages cleanly, then a byte changes on
    // disk. The stub must refuse by name and leave the live tree untouched.
    let archive = tree_artifact("0.0.3", &probe_bytes);
    let (objects, resolved) = resolve_sealed_tree_release("0.0.3", &archive);
    lait::update::tree::stage_tree_with(
        |asked, _limit| {
            objects.get(asked).cloned().ok_or_else(|| {
                lait::update::feed::Failure::Unreachable(format!("no object at {asked}"))
            })
        },
        &resolved,
        tree_target(),
        root,
    )
    .expect("the 0.0.3 tree stages");
    std::fs::write(root.join("staged").join("version.txt"), "0.0.3-tampered").expect("the tamper");
    run_stub(root);
    assert_eq!(
        wait_for_announcement(root).trim(),
        "0.0.2",
        "a tampered stage was applied, or the launch did not survive the refusal"
    );
    let log = std::fs::read_to_string(root.join("stub.log")).expect("the stub said its refusals");
    assert!(
        log.contains("verification failed"),
        "the tamper refusal must name verification, not fail vaguely: {log}"
    );
}

/// The evergreen restart, end to end: a release staged while the client ran
/// becomes the live tree with nobody launching anything. The client writes
/// the relaunch request and exits; the stub — still holding the claim it
/// took at the first launch — consumes the request, applies, and starts the
/// new tree with the requested version in the relaunch env. This is the
/// window a self-relaunch can never reach, because the stub is waiting on
/// the very process that wants to move.
///
/// The request path and the env name are passed in from the *client's*
/// constants (`client::update`), so the run also welds the mirrored
/// vocabulary: the stub honouring this request is the two crates agreeing.
#[test]
fn a_relaunch_request_reaches_the_apply_window_under_one_stub() {
    let stub = stub_binary();
    let probe = probe_binary();
    let probe_bytes = std::fs::read(&probe).expect("the probe binary's bytes");

    let scratch = tempfile::tempdir().expect("an install root");
    let root = scratch.path();
    std::fs::copy(&stub, root.join(installed_stub_name()))
        .expect("the stub lands in the install root");

    let current = root.join("current");
    std::fs::create_dir(&current).expect("the live tree");
    std::fs::copy(&probe, current.join(tree_entry_name())).expect("the live entry");
    std::fs::write(current.join("version.txt"), "0.0.1").expect("the live version");

    let runs = root.join("runs.log");
    let gate = root.join("relaunch.gate");
    let mut stub_process = Command::new(root.join(installed_stub_name()))
        .env("CHAIN_PROBE_ANNOUNCE", root)
        .env("CHAIN_PROBE_RUNS", &runs)
        .env(
            "CHAIN_PROBE_ENV_NAME",
            astrolabe::client::update::RELAUNCHED_ENV,
        )
        .env("CHAIN_PROBE_RELAUNCH_ONCE", root.join("relaunch.asked"))
        .env("CHAIN_PROBE_RELAUNCH_GATE", &gate)
        .env(
            "CHAIN_PROBE_REQUEST",
            root.join(astrolabe::client::update::RELAUNCH_REQUEST),
        )
        .env("CHAIN_PROBE_REQUEST_BODY", "0.0.2")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the stub spawns");

    // The old tree came up and is holding at the gate.
    assert_eq!(
        wait_for_announcement(root).trim(),
        "0.0.1",
        "the wrong tree came up first"
    );

    // 0.0.2 lands while the client runs, staged exactly as the daemon would.
    let archive = tree_artifact("0.0.2", &probe_bytes);
    let (objects, resolved) = resolve_sealed_tree_release("0.0.2", &archive);
    lait::update::tree::stage_tree_with(
        |asked, _limit| {
            objects.get(asked).cloned().ok_or_else(|| {
                lait::update::feed::Failure::Unreachable(format!("no object at {asked}"))
            })
        },
        &resolved,
        tree_target(),
        root,
    )
    .expect("the 0.0.2 tree stages under a live client");

    // Open the gate: the client asks for the window and exits.
    std::fs::write(&gate, b"go").expect("the gate opens");

    assert_eq!(
        wait_for_announcement(root).trim(),
        "0.0.2",
        "the relaunch did not come up on the staged release"
    );
    let status = stub_process.wait().expect("the stub is waited on");
    assert!(
        status.success(),
        "a clean relaunch chain still exited with a failure"
    );
    assert!(
        !root
            .join(astrolabe::client::update::RELAUNCH_REQUEST)
            .exists(),
        "the request survived being answered, which is a relaunch loop"
    );
    assert_eq!(
        std::fs::read_to_string(&runs).expect("the runs log"),
        "0.0.1 env=-\n0.0.2 env=0.0.2\n",
        "the answering launch did not carry the requested version, or extra launches happened"
    );
}

/// A second shell/protocol launch may start while the primary stub is waiting
/// on its client, but it does not own the installation. It must not consume
/// the request that tells the primary to open the apply window — neither at
/// secondary startup nor after the secondary client exits.
#[test]
fn a_secondary_stub_cannot_consume_the_primary_relaunch_request() {
    let stub = stub_binary();
    let probe = probe_binary();

    let scratch = tempfile::tempdir().expect("an install root");
    let root = scratch.path();
    std::fs::copy(&stub, root.join(installed_stub_name()))
        .expect("the stub lands in the install root");
    let current = root.join("current");
    std::fs::create_dir(&current).expect("the live tree");
    std::fs::copy(&probe, current.join(tree_entry_name())).expect("the live entry");
    std::fs::write(current.join("version.txt"), "0.0.1").expect("the live version");

    let gate = root.join("relaunch.gate");
    let request = root.join(astrolabe::client::update::RELAUNCH_REQUEST);
    let mut primary = Command::new(root.join(installed_stub_name()))
        .env("CHAIN_PROBE_ANNOUNCE", root)
        .env("CHAIN_PROBE_RELAUNCH_ONCE", root.join("relaunch.asked"))
        .env("CHAIN_PROBE_RELAUNCH_GATE", &gate)
        .env("CHAIN_PROBE_REQUEST", &request)
        .env("CHAIN_PROBE_REQUEST_BODY", "0.0.9")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the primary stub starts");
    assert_eq!(wait_for_announcement(root).trim(), "0.0.1");

    std::fs::write(&request, "0.0.9").expect("the primary's relaunch request");
    let secondary = Command::new(root.join(installed_stub_name()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the secondary stub runs");
    assert!(secondary.success(), "the secondary launch failed");
    assert_eq!(
        std::fs::read_to_string(&request).expect("the request survived the secondary"),
        "0.0.9",
        "a claimless secondary consumed the primary's relaunch request"
    );

    std::fs::write(&gate, b"go").expect("the primary client exits");
    assert!(
        primary.wait().expect("the primary stub exits").success(),
        "the primary did not answer its own relaunch request"
    );
    assert!(
        !request.exists(),
        "the owning primary did not consume the request"
    );
}

/// A request with nothing staged is still answered — the person asked to
/// restart, and turning that into a quit would be the launcher refusing to
/// launch — but answered exactly once: consuming the request is what bounds
/// the loop, and the plain exit then passes through.
#[test]
fn a_request_with_nothing_staged_relaunches_once_and_is_consumed() {
    let stub = stub_binary();
    let probe = probe_binary();

    let scratch = tempfile::tempdir().expect("an install root");
    let root = scratch.path();
    std::fs::copy(&stub, root.join(installed_stub_name()))
        .expect("the stub lands in the install root");

    let current = root.join("current");
    std::fs::create_dir(&current).expect("the live tree");
    std::fs::copy(&probe, current.join(tree_entry_name())).expect("the live entry");
    std::fs::write(current.join("version.txt"), "0.0.1").expect("the live version");

    let runs = root.join("runs.log");
    let status = Command::new(root.join(installed_stub_name()))
        .env("CHAIN_PROBE_RUNS", &runs)
        .env(
            "CHAIN_PROBE_ENV_NAME",
            astrolabe::client::update::RELAUNCHED_ENV,
        )
        .env("CHAIN_PROBE_RELAUNCH_ONCE", root.join("relaunch.asked"))
        .env(
            "CHAIN_PROBE_REQUEST",
            root.join(astrolabe::client::update::RELAUNCH_REQUEST),
        )
        .env("CHAIN_PROBE_REQUEST_BODY", "0.0.9")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the stub runs the whole exchange");

    assert!(
        status.success(),
        "the final plain exit did not pass through"
    );
    assert_eq!(
        std::fs::read_to_string(&runs).expect("the runs log"),
        "0.0.1 env=-\n0.0.1 env=0.0.9\n",
        "one request must mean exactly one answering launch"
    );
    assert!(
        !root
            .join(astrolabe::client::update::RELAUNCH_REQUEST)
            .exists(),
        "an answered request was left behind"
    );
}
