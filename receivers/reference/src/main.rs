#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::unchecked_time_subtraction,
        clippy::todo,
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::panic,
        clippy::exit,
        clippy::as_conversions
    )
)]

mod client;
mod playback;
mod state;
mod tls;
mod transport;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use display_protocol::bounds::{
    MAX_ASSET_BYTES, MAX_PAIRING_BODY_BYTES, MAX_PROGRAM_ITEMS, MAX_STAGED_BYTES,
    MAX_STAGING_HORIZON_MS,
};
use display_protocol::pairing::ReceiverBootstrap;
use display_protocol::program::DisplayAssetMediaType;
use display_protocol::receiver::{
    AccessibilityCapabilities, HealthGranularity, LatencyClass, PlaybackCapabilities, PlaybackTier,
    ReceiverCapabilities, ReceiverPlatform, SyncClass, Viewport,
};

struct Options {
    bootstrap: PathBuf,
    state: PathBuf,
    output: PathBuf,
    width: u32,
    height: u32,
    locale: String,
}

enum Command {
    Run(Options),
    Help,
}

fn main() -> Result<()> {
    let command = parse_args(std::env::args().skip(1))?;
    let Command::Run(options) = command else {
        print_usage();
        return Ok(());
    };
    let bootstrap = read_bootstrap(&options.bootstrap)?;
    let capabilities = capabilities(options.width, options.height, options.locale);
    let mut receiver =
        client::ReferenceReceiver::open(bootstrap, options.state, options.output, capabilities)?;
    receiver.run()
}

fn parse_args(mut arguments: impl Iterator<Item = String>) -> Result<Command> {
    let mut bootstrap = None;
    let mut state = None;
    let mut output = None;
    let mut width = 1_920;
    let mut height = 1_080;
    let mut locale = "en-US".to_owned();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--bootstrap" => {
                bootstrap = Some(PathBuf::from(next_value(&mut arguments, &argument)?))
            }
            "--state" => state = Some(PathBuf::from(next_value(&mut arguments, &argument)?)),
            "--output" => output = Some(PathBuf::from(next_value(&mut arguments, &argument)?)),
            "--width" => {
                width = next_value(&mut arguments, &argument)?
                    .parse()
                    .context("parse --width")?;
            }
            "--height" => {
                height = next_value(&mut arguments, &argument)?
                    .parse()
                    .context("parse --height")?;
            }
            "--locale" => locale = next_value(&mut arguments, &argument)?,
            "--help" | "-h" => return Ok(Command::Help),
            _ => bail!("unknown receiver argument: {argument}"),
        }
    }
    Ok(Command::Run(Options {
        bootstrap: bootstrap.ok_or_else(|| anyhow!("--bootstrap is required"))?,
        state: state.ok_or_else(|| anyhow!("--state is required"))?,
        output: output.ok_or_else(|| anyhow!("--output is required"))?,
        width,
        height,
        locale,
    }))
}

fn next_value(arguments: &mut impl Iterator<Item = String>, option: &str) -> Result<String> {
    arguments
        .next()
        .ok_or_else(|| anyhow!("{option} requires a value"))
}

fn read_bootstrap(path: &Path) -> Result<ReceiverBootstrap> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read receiver bootstrap {}", path.display()))?;
    if bytes.len() > MAX_PAIRING_BODY_BYTES {
        bail!("receiver bootstrap exceeds its protocol byte bound");
    }
    serde_json::from_slice(&bytes).context("decode receiver bootstrap")
}

fn capabilities(width: u32, height: u32, locale: String) -> ReceiverCapabilities {
    ReceiverCapabilities {
        protocol_major: display_protocol::PROTOCOL_MAJOR,
        platform: ReceiverPlatform::Desktop,
        build: env!("CARGO_PKG_VERSION").to_owned(),
        viewport: Viewport {
            width,
            height,
            scale_milli: 1_000,
        },
        image_types: vec![DisplayAssetMediaType::ImagePng],
        max_asset_bytes: MAX_ASSET_BYTES,
        max_staged_bytes: MAX_STAGED_BYTES,
        max_program_items: u16::try_from(MAX_PROGRAM_ITEMS).unwrap_or(u16::MAX),
        max_staging_horizon_ms: MAX_STAGING_HORIZON_MS,
        locale,
        accessibility: AccessibilityCapabilities {
            native_screen_reader: false,
            spoken_summary: false,
            captions: false,
            audio_description: false,
        },
        // The native-HLS profile, with the coherence `validate_capabilities`
        // demands of it: no positional sync, no rate-control claim, coarse
        // health. The playback itself is an atomic handoff — the presenter
        // writes the ticketed playlist URL and whatever consumes the output
        // directory plays it — which is exactly the shape this receiver's
        // frame presentation already has.
        playback: PlaybackCapabilities {
            tier: PlaybackTier::NativeHls,
            sync_class: SyncClass::None,
            rate_control_probed: false,
            latency_class: LatencyClass::Broadcast,
            health_granularity: HealthGranularity::Coarse,
        },
    }
}

fn print_usage() {
    let usage = [
        "Astrolabe Display reference receiver",
        "",
        "Usage:",
        "  astrolabe-display-reference \\",
        "    --bootstrap <bootstrap.json> \\",
        "    --state <private-state-directory> \\",
        "    --output <presentation-directory> \\",
        "    [--width 1920] [--height 1080] [--locale en-US]",
        "",
        "The presentation directory exposes active.json and, for a frame scene, frame.png.",
    ]
    .join("\n");
    println!("{usage}");
}
