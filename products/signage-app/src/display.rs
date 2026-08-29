#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "bounded raster coordinates are checked before image access"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::sync::Arc;

use font8x8::{UnicodeFonts, BASIC_FONTS};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use signage::contract::{MediaSource, SignageMedia};
use world_interface::display::{
    BlankReason, CanonicalDisplayInput, DisplayAssessment, DisplayChoice, DisplayChoices,
    DisplayOutputKind, DisplayProjection, DisplayRenderer, DisplayRequest, DisplayResourceId,
    DisplaySurface, DisplaySurfaceDescriptor, DisplaySurfaceId, FrameMediaType, MediaOrigin,
    MediaProtocol, ProgramCycle, RenderedFrame, RenderedMedia, RenderedProgram,
    RenderedProgramItem, RenderedScene,
};
use world_interface::{ClientAccess, ClientInvocation, Failure};

const SURFACE_ID: &str = "signage.program";
/// How long an item plays when neither it nor its library entry says. The
/// editor shows the same ten seconds, so what a person saw on the strip is
/// what the screen does. Without this a video whose length nobody recorded
/// went out open-ended, which the receiver contract allows only for a held
/// last item — and one such clip in a loop failed the whole program.
const DEFAULT_ITEM_DURATION_MS: u32 = 10_000;
const MAX_RENDER_WIDTH: u32 = 4_096;
const MAX_RENDER_HEIGHT: u32 = 2_160;

/// What a receiver is pointed at.
///
/// The screen, not the program. Naming a program here made every broadcast a
/// fan-out: to interrupt a fleet you had to re-issue an assignment per panel,
/// and a grant that carried the answer was a snapshot of it. Pointing at the
/// screen moves resolution behind the one query a prepare is allowed, so an
/// emergency is a single Body write and every receiver picks it up on the
/// doorbell it is already waiting on.
#[derive(Debug, Serialize, Deserialize)]
struct ScreenInput {
    screen: String,
}

pub fn program_surface() -> Result<DisplaySurface, Failure> {
    let world = signage::contract::world_id();
    let mut input_digest = Sha256::new();
    input_digest.update(b"signage.program.input.v2:{screen:body-id}");
    let mut renderer_identity = Sha256::new();
    renderer_identity.update(
        b"signage.program.renderer.v8:font8x8:png:library:content:lait-live:channels:broadcasts:place-facts-preset",
    );
    let mut descriptor = DisplaySurfaceDescriptor {
        id: DisplaySurfaceId::new(SURFACE_ID)?,
        title: "Signage program".into(),
        runtime_implementation: crate::implementation_id(),
        contract_version: 4,
        input_contract_digest: input_digest.finalize().into(),
        renderer_identity: renderer_identity.finalize().into(),
        contract_digest: [0; 32],
        outputs: BTreeSet::from([DisplayOutputKind::Frame, DisplayOutputKind::Media]),
    };
    descriptor.contract_digest = descriptor.expected_contract_digest(&world);
    Ok(DisplaySurface::local(
        descriptor,
        canonicalize_input,
        prepare,
        Arc::new(SignageRenderer),
    )
    .with_choices(DisplayChoices {
        prepare: choices_prepare,
        project: choices_project,
    }))
}

/// What a television can be pointed at: the screens this Space has.
fn choices_prepare(surface: &DisplaySurfaceId) -> Result<ClientInvocation, Failure> {
    if surface.as_str() != SURFACE_ID {
        return Err(Failure::new("Signage listing received another surface"));
    }
    let call = crate::encode_call(&crate::SignageRequest::ScreenList)
        .map_err(|error| Failure::new(error.to_string()))?;
    Ok(ClientInvocation::world(call, ClientAccess::Query, None))
}

fn choices_project(value: Value) -> Result<Vec<DisplayChoice>, Failure> {
    let response: crate::SignageResponse = serde_json::from_value(value)
        .map_err(|error| Failure::new(format!("decode Signage screen list: {error}")))?;
    let crate::SignageResponse::Screens { screens } = response else {
        return Err(Failure::new("Signage did not answer with its screens"));
    };
    Ok(screens
        .into_iter()
        .map(|screen| DisplayChoice {
            id: screen.id,
            title: screen.name,
        })
        .collect())
}

fn canonicalize_input(value: Value) -> Result<CanonicalDisplayInput, Failure> {
    let input: ScreenInput = serde_json::from_value(value)
        .map_err(|error| Failure::new(format!("invalid Signage display input: {error}")))?;
    if replica::body::BodyId::parse(&input.screen).is_none() {
        return Err(Failure::new("invalid Signage screen id"));
    }
    let bytes = serde_json::to_vec(&input)
        .map_err(|error| Failure::new(format!("encode Signage display input: {error}")))?;
    CanonicalDisplayInput::new(bytes)
}

fn prepare(request: &DisplayRequest) -> Result<ClientInvocation, Failure> {
    request.validate()?;
    if request.surface.as_str() != SURFACE_ID {
        return Err(Failure::new("Signage renderer received another surface"));
    }
    let input: ScreenInput = serde_json::from_slice(request.input.as_bytes())
        .map_err(|error| Failure::new(format!("decode Signage display input: {error}")))?;
    let call = crate::encode_call(&crate::SignageRequest::ScreenPlays {
        screen: input.screen,
    })
    .map_err(|error| Failure::new(error.to_string()))?;
    Ok(ClientInvocation::world(call, ClientAccess::Query, None))
}

struct SignageRenderer;

impl DisplayRenderer for SignageRenderer {
    fn project<'a>(
        &'a self,
        value: Value,
        request: &'a DisplayRequest,
    ) -> world_interface::display::DisplayProjectFuture<'a> {
        Box::pin(async move {
            if request.width > MAX_RENDER_WIDTH || request.height > MAX_RENDER_HEIGHT {
                return Err(Failure::new(
                    "Signage render dimensions exceed the frame bound",
                ));
            }
            let response: crate::SignageResponse = serde_json::from_value(value)
                .map_err(|error| Failure::new(format!("decode Signage projection: {error}")))?;
            let crate::SignageResponse::Plays {
                screen: Some(screen),
                channels,
                broadcasts,
                audiences,
                programs,
                media,
                presets,
            } = response
            else {
                return Err(Failure::new("Signage screen is unavailable"));
            };
            let now_unix_ms = request
                .window_start_unix
                .checked_mul(1_000)
                .ok_or_else(|| Failure::new("Signage schedule time overflowed"))?;

            // The context a screen reports about itself is not in hand here —
            // a prepare reads, it does not sample — so an `Observed` audience
            // reaches nobody from this side. Absent, never false.
            let lookup: BTreeMap<String, signage::Match> = audiences
                .iter()
                .map(|entry| (entry.id.clone(), entry.rule.clone()))
                .collect();
            let cx = signage::Context::at(now_unix_ms);
            let playback = signage::fleet::resolve(&screen, &channels, &broadcasts, &cx, &lookup);

            let chosen = match &playback.showing {
                signage::Showing::Program { program } => {
                    programs.iter().find(|candidate| &candidate.id == program)
                }
                // Told to go dark, or told something this build does not draw.
                // Both are answers, and neither is an error to report.
                signage::Showing::Blank
                | signage::Showing::Unaddressed
                | signage::Showing::Kind { .. } => None,
            };
            let library: BTreeMap<&str, &SignageMedia> = media
                .iter()
                .map(|entry| (entry.id.as_str(), entry))
                .collect();
            let scheduled = match chosen {
                Some(program) if program.validate() => {
                    Some(program.scheduled_at(now_unix_ms).map_err(|error| {
                        Failure::new(format!("evaluate Signage schedule: {error}"))
                    })?)
                }
                _ => None,
            };
            let (scheduled_items, schedule_boundary) = match &scheduled {
                Some(scheduled) => (scheduled.items.clone(), scheduled.next_boundary_unix_ms),
                None => (Vec::new(), None),
            };
            let idle = scheduled_items.is_empty();
            let mut items = Vec::with_capacity(scheduled_items.len().max(1));
            let mut kind_refresh_unix_ms = None;
            for item in scheduled_items {
                let entry = library.get(item.media.as_str()).copied();
                let live = resolved_entry(entry, &screen, &presets);
                let drawn = scene(live.as_ref(), request.width, request.height, now_unix_ms)?;
                kind_refresh_unix_ms = match (kind_refresh_unix_ms, drawn.refresh_unix_ms) {
                    (None, next) => next,
                    (Some(left), Some(right)) => Some(left.min(right)),
                    (Some(left), None) => Some(left),
                };
                items.push(RenderedProgramItem {
                    id: item.id.clone(),
                    duration_ms: Some(item_duration_ms(
                        item.duration_ms,
                        live.as_ref().and_then(|entry| entry.duration_ms),
                    )),
                    scene: drawn.scene,
                    assessment: DisplayAssessment::Current,
                    spoken_summary: spoken_summary(live.as_ref(), now_unix_ms),
                });
            }
            if idle {
                items.push(RenderedProgramItem {
                    id: "schedule-idle".into(),
                    duration_ms: None,
                    scene: RenderedScene::Blank(BlankReason::ProgramEnded),
                    assessment: DisplayAssessment::Current,
                    spoken_summary: Some("No content is scheduled".into()),
                });
            }
            let refresh_after_ms = [
                schedule_boundary,
                kind_refresh_unix_ms,
                playback.next_boundary_unix_ms,
            ]
            .into_iter()
            .flatten()
            .map(|boundary| boundary.saturating_sub(now_unix_ms).max(1))
            .min()
            .and_then(|delay| {
                u32::try_from(delay)
                    .ok()
                    .filter(|delay| *delay <= request.window_horizon_ms)
            });
            Ok(DisplayProjection {
                program: RenderedProgram {
                    items,
                    cycle: match chosen {
                        Some(program) if !idle => cycle(program.cycle),
                        // Nothing to cycle through: hold, rather than blank on
                        // a boundary nobody set.
                        _ => ProgramCycle::HoldLast,
                    },
                    refresh_after_ms,
                },
                assessment: DisplayAssessment::Current,
                // Why this screen is showing this, in the words the source
                // used, so an operator hears the same sentence the interface
                // shows them.
                spoken_summary: Some(match (&playback.source, chosen) {
                    (Some(signage::Resolved::Broadcast { name, .. }), _) => name.clone(),
                    (_, Some(program)) => program.name.clone(),
                    (Some(signage::Resolved::Channel { name, .. }), None) => name.clone(),
                    (None, None) => "Nothing is addressed to this screen".into(),
                }),
            })
        })
    }
}

/// The item's own length, else its entry's, else the default — never open.
fn item_duration_ms(item: Option<u32>, entry: Option<u32>) -> u32 {
    item.or(entry).unwrap_or(DEFAULT_ITEM_DURATION_MS)
}

fn cycle(value: signage::ProgramCycle) -> ProgramCycle {
    match value {
        signage::ProgramCycle::HoldLast => ProgramCycle::HoldLast,
        signage::ProgramCycle::Loop => ProgramCycle::Loop,
        signage::ProgramCycle::PollAtEnd => ProgramCycle::PollAtEnd,
        signage::ProgramCycle::BlankAtEnd => ProgramCycle::BlankAtEnd,
    }
}

/// What one library entry looks like on a screen.
///
/// An entry this renderer cannot present blanks, and the rest of the program
/// still plays. That covers a dangling item, whose reference resolved to
/// nothing, and a kind this app does not draw. Athan is drawn here because
/// this application owns that kind.
struct DrawnScene {
    scene: RenderedScene,
    refresh_unix_ms: Option<u64>,
}

fn drawn(scene: RenderedScene) -> DrawnScene {
    DrawnScene {
        scene,
        refresh_unix_ms: None,
    }
}

/// One kind entry, resolved against where it is playing.
///
/// Three layers, and they are ordered by how widely each varies. The preset is
/// the presentation, shared by every entry that points at it. The entry's own
/// settings sit over that. The screen's facts sit over *both*, because what a
/// congregation practises and where a panel stands are the narrowest truths in
/// the stack and the only ones that make one card correct in two cities.
///
/// Geography rides separately, as typed fields, because every location-aware
/// kind wants it and none of them agree on what to compute from it.
fn resolved_entry(
    entry: Option<&SignageMedia>,
    screen: &signage::SignageScreen,
    presets: &[signage::contract::SignagePreset],
) -> Option<SignageMedia> {
    let entry = entry?;
    let MediaSource::Kind {
        kind,
        preset,
        settings,
    } = &entry.source
    else {
        return Some(entry.clone());
    };

    let mut resolved: BTreeMap<String, String> = preset
        .as_ref()
        .and_then(|id| presets.iter().find(|candidate| &candidate.id == id))
        .filter(|candidate| &candidate.kind == kind)
        .map(|candidate| candidate.settings.clone())
        .unwrap_or_default();
    resolved.extend(settings.clone());
    if let Some(facts) = screen.facts.get(kind) {
        resolved.extend(facts.clone());
    }
    if let Some(place) = &screen.place {
        resolved.insert("latitude".into(), place.latitude.to_string());
        resolved.insert("longitude".into(), place.longitude.to_string());
        resolved.insert("timezone".into(), place.timezone.clone());
    }

    Some(SignageMedia {
        source: MediaSource::Kind {
            kind: kind.clone(),
            preset: preset.clone(),
            settings: resolved,
        },
        ..entry.clone()
    })
}

fn scene(
    entry: Option<&SignageMedia>,
    width: u32,
    height: u32,
    now_unix_ms: u64,
) -> Result<DrawnScene, Failure> {
    let Some(entry) = entry else {
        return Ok(drawn(RenderedScene::Blank(BlankReason::Unsupported)));
    };
    match &entry.source {
        MediaSource::Card {
            title,
            body,
            background,
            foreground,
        } => Ok(drawn(RenderedScene::Frame(RenderedFrame {
            media_type: FrameMediaType::Png,
            width,
            height,
            bytes: render_card(title, body, background, foreground, width, height)?,
        }))),
        MediaSource::Stored { .. } => {
            Ok(drawn(entry.source.content_ref().map_or(
                RenderedScene::Blank(BlankReason::Unsupported),
                |content| media_scene(MediaOrigin::Stored(content)),
            )))
        }
        MediaSource::Live { resource } => {
            Ok(drawn(DisplayResourceId::new(resource).map_or(
                RenderedScene::Blank(BlankReason::Unsupported),
                |resource| media_scene(MediaOrigin::Live(resource)),
            )))
        }
        MediaSource::Kind { kind, settings, .. } => {
            athan_scene(kind, settings, width, height, now_unix_ms)
        }
    }
}

fn athan_scene(
    kind: &str,
    settings: &BTreeMap<String, String>,
    width: u32,
    height: u32,
    now_unix_ms: u64,
) -> Result<DrawnScene, Failure> {
    if !crate::athan::kind_is_athan(kind) {
        return Ok(drawn(RenderedScene::Blank(BlankReason::Unsupported)));
    }
    let Some(day) = crate::athan::times_from_settings(settings, now_unix_ms) else {
        return Ok(drawn(RenderedScene::Blank(BlankReason::Unsupported)));
    };
    Ok(DrawnScene {
        scene: RenderedScene::Frame(RenderedFrame {
            media_type: FrameMediaType::Png,
            width,
            height,
            bytes: render_athan(&day, width, height)?,
        }),
        refresh_unix_ms: Some(day.next_change_unix_ms),
    })
}

/// HLS is the only transport a receiver can declare without also declaring live
/// positional sync.
///
/// A name neither namespace can carry blanks rather than failing the render.
fn media_scene(origin: MediaOrigin) -> RenderedScene {
    RenderedScene::Media(RenderedMedia {
        protocol: MediaProtocol::Hls,
        origin,
    })
}

/// A card speaks the words it was authored with; everything else speaks the
/// name the library gave it, which is the only name a listener would recognise.
fn spoken_summary(entry: Option<&SignageMedia>, now_unix_ms: u64) -> Option<String> {
    let entry = entry?;
    match &entry.source {
        MediaSource::Card { title, body, .. } => Some(if body.trim().is_empty() {
            title.clone()
        } else {
            format!("{title}. {body}")
        }),
        MediaSource::Kind { kind, settings, .. } if crate::athan::kind_is_athan(kind) => {
            crate::athan::times_from_settings(settings, now_unix_ms).and_then(|day| {
                Some(match day.phase {
                    crate::athan::Phase::Countdown { label, remain_s } => {
                        format!("{label} in {remain_s} seconds.")
                    }
                    crate::athan::Phase::Silence => "Prayer in progress.".into(),
                    crate::athan::Phase::Table => {
                        let clock = day.next_event_clock()?;
                        let when = crate::athan::format_clock(clock, day.clock_24h);
                        if day.next_is_iqamah {
                            format!("Prayer times. Next is Iqamah at {when}.")
                        } else {
                            let next = day.prayers.get(day.next)?;
                            format!("Prayer times. Next is {} at {when}.", next.name)
                        }
                    }
                })
            })
        }
        _ => Some(entry.name.clone()),
    }
}

fn theme_colors(theme: crate::athan::Theme) -> (Rgba<u8>, Rgba<u8>, Rgba<u8>) {
    match theme {
        crate::athan::Theme::Ink => (
            Rgba([18, 14, 10, 255]),
            Rgba([180, 160, 130, 255]),
            Rgba([243, 192, 122, 255]),
        ),
        crate::athan::Theme::Paper => (
            Rgba([246, 241, 232, 255]),
            Rgba([90, 78, 64, 255]),
            Rgba([36, 28, 20, 255]),
        ),
        crate::athan::Theme::Emerald => (
            Rgba([12, 36, 28, 255]),
            Rgba([150, 180, 160, 255]),
            Rgba([220, 232, 210, 255]),
        ),
        crate::athan::Theme::Night => (
            Rgba([8, 10, 16, 255]),
            Rgba([140, 150, 170, 255]),
            Rgba([210, 220, 235, 255]),
        ),
    }
}

fn render_athan(day: &crate::athan::DayTimes, width: u32, height: u32) -> Result<Vec<u8>, Failure> {
    let (background, muted, accent) = theme_colors(day.theme);
    let mut image = RgbaImage::from_pixel(width, height, background);
    let inset = width.min(height) / 12;
    match day.phase {
        crate::athan::Phase::Silence => {
            let scale = (width / 160).min(height / 60).clamp(3, 12);
            draw_line(
                &mut image,
                "Prayer in progress",
                inset,
                height / 2 - scale.saturating_mul(4),
                scale,
                accent,
            );
        }
        crate::athan::Phase::Countdown { label, remain_s } => {
            let title_scale = (width / 160).min(height / 70).clamp(3, 12);
            draw_line(&mut image, label, inset, height / 5, title_scale, muted);
            let mins = remain_s / 60;
            let secs = remain_s % 60;
            let clock = format!("{mins}:{secs:02}");
            draw_line(
                &mut image,
                &clock,
                inset,
                height / 5 + title_scale.saturating_mul(14),
                title_scale.saturating_add(4).min(16),
                accent,
            );
        }
        crate::athan::Phase::Table => {
            let title_scale = (width / 220).min(height / 90).clamp(2, 10);
            draw_line(&mut image, "Athan", inset, height / 12, title_scale, accent);
            let mut sub = day.now_label.clone();
            if day.show_hijri && !day.hijri_label.is_empty() {
                sub = format!("{}  {}", sub, day.hijri_label);
            }
            draw_line(
                &mut image,
                &sub,
                inset,
                height / 12 + title_scale.saturating_mul(12),
                title_scale.saturating_sub(1).max(2),
                muted,
            );
            let row_scale = (width / 300).min(height / 120).clamp(2, 8);
            let row_height = row_scale.saturating_mul(12);
            let start_y = height / 12
                + title_scale.saturating_mul(12)
                + title_scale.saturating_mul(8)
                + row_height;
            let iqamah_x = width
                .saturating_sub(inset)
                .saturating_sub(row_scale.saturating_mul(9).saturating_mul(5));
            let adhan_x = if day.show_iqamah {
                iqamah_x.saturating_sub(row_scale.saturating_mul(9).saturating_mul(6))
            } else {
                iqamah_x
            };
            for (index, prayer) in day.prayers.iter().enumerate() {
                let y = start_y
                    .saturating_add(row_height.saturating_mul(u32::try_from(index).unwrap_or(0)));
                let color = if index == day.next { accent } else { muted };
                draw_line(&mut image, prayer.name, inset, y, row_scale, color);
                let adhan = crate::athan::format_clock(prayer.adhan, day.clock_24h);
                draw_line(&mut image, &adhan, adhan_x, y, row_scale, color);
                if day.show_iqamah {
                    let iqamah = prayer.iqamah.map_or_else(
                        || "--:--".into(),
                        |clock| crate::athan::format_clock(clock, day.clock_24h),
                    );
                    draw_line(&mut image, &iqamah, iqamah_x, y, row_scale, color);
                }
            }
        }
    }
    encode_png(image)
}

fn draw_line(image: &mut RgbaImage, text: &str, x: u32, y: u32, scale: u32, color: Rgba<u8>) {
    let cell = scale.saturating_mul(9).max(1);
    for (column, character) in text.chars().enumerate() {
        let line_x = x.saturating_add(
            u32::try_from(column)
                .unwrap_or(u32::MAX)
                .saturating_mul(cell),
        );
        if line_x >= image.width() {
            break;
        }
        draw_character(image, character, line_x, y, scale, color);
    }
}

fn encode_png(image: RgbaImage) -> Result<Vec<u8>, Failure> {
    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(|error| Failure::new(format!("encode Signage frame: {error}")))?;
    Ok(cursor.into_inner())
}

fn render_card(
    title: &str,
    body: &str,
    background: &str,
    foreground: &str,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, Failure> {
    let background = rgb(background)?;
    let foreground = rgb(foreground)?;
    let mut image = RgbaImage::from_pixel(width, height, background);
    let inset = width.min(height) / 12;
    let title_scale = (width / 180).min(height / 80).clamp(2, 12);
    let body_scale = title_scale.saturating_sub(2).max(2);
    let title_y = height / 4;
    draw_wrapped(
        &mut image,
        title,
        inset,
        title_y,
        width.saturating_sub(inset.saturating_mul(2)),
        title_scale,
        foreground,
        2,
    );
    let body_y = title_y.saturating_add(title_scale.saturating_mul(11));
    draw_wrapped(
        &mut image,
        body,
        inset,
        body_y,
        width.saturating_sub(inset.saturating_mul(2)),
        body_scale,
        foreground,
        5,
    );
    encode_png(image)
}

fn rgb(value: &str) -> Result<Rgba<u8>, Failure> {
    if value.len() != 6 {
        return Err(Failure::new("invalid Signage RGB color"));
    }
    let parse = |start: usize| {
        value
            .get(start..start + 2)
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
    };
    match (parse(0), parse(2), parse(4)) {
        (Some(red), Some(green), Some(blue)) => Ok(Rgba([red, green, blue, 255])),
        _ => Err(Failure::new("invalid Signage RGB color")),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_wrapped(
    image: &mut RgbaImage,
    text: &str,
    x: u32,
    y: u32,
    width: u32,
    scale: u32,
    color: Rgba<u8>,
    max_lines: usize,
) {
    let cell = scale.saturating_mul(9).max(1);
    let columns = usize::try_from(width / cell).unwrap_or(1).max(1);
    let mut lines = Vec::<String>::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let extra = usize::from(!line.is_empty());
        if !line.is_empty() && line.chars().count() + extra + word.chars().count() > columns {
            lines.push(std::mem::take(&mut line));
            if lines.len() == max_lines {
                break;
            }
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.extend(
            word.chars()
                .take(columns.saturating_sub(line.chars().count())),
        );
    }
    if lines.len() < max_lines && !line.is_empty() {
        lines.push(line);
    }
    for (line_index, line) in lines.iter().enumerate() {
        let line_y = y.saturating_add(
            u32::try_from(line_index)
                .unwrap_or(u32::MAX)
                .saturating_mul(scale.saturating_mul(10)),
        );
        for (column, character) in line.chars().enumerate() {
            let line_x = x.saturating_add(
                u32::try_from(column)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(cell),
            );
            draw_character(image, character, line_x, line_y, scale, color);
        }
    }
}

fn draw_character(
    image: &mut RgbaImage,
    character: char,
    x: u32,
    y: u32,
    scale: u32,
    color: Rgba<u8>,
) {
    let glyph = BASIC_FONTS
        .get(character)
        .or_else(|| BASIC_FONTS.get('?'))
        .unwrap_or([0; 8]);
    for (row, bits) in glyph.into_iter().enumerate() {
        for column in 0..8u32 {
            if bits & (1u8 << column) == 0 {
                continue;
            }
            let pixel_x = x.saturating_add(column.saturating_mul(scale));
            let pixel_y =
                y.saturating_add(u32::try_from(row).unwrap_or(u32::MAX).saturating_mul(scale));
            for dy in 0..scale {
                for dx in 0..scale {
                    let target_x = pixel_x.saturating_add(dx);
                    let target_y = pixel_y.saturating_add(dy);
                    if target_x < image.width() && target_y < image.height() {
                        image.put_pixel(target_x, target_y, color);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_of_unknown_length_plays_the_default_rather_than_open_ending_the_program() {
        assert_eq!(item_duration_ms(Some(4_000), Some(9_700)), 4_000);
        assert_eq!(item_duration_ms(None, Some(9_700)), 9_700);
        assert_eq!(item_duration_ms(None, None), DEFAULT_ITEM_DURATION_MS);
    }

    #[test]
    fn the_surface_lists_its_screens_as_choices_by_their_names() {
        let invocation = choices_prepare(&DisplaySurfaceId::new(SURFACE_ID).unwrap()).unwrap();
        assert_eq!(invocation.access(), ClientAccess::Query);
        assert!(choices_prepare(&DisplaySurfaceId::new("signage.other").unwrap()).is_err());

        let listed = choices_project(serde_json::json!({
            "kind": "screens",
            "screens": [
                { "id": "bod_lobby", "name": "Lobby" },
                { "id": "bod_cafe", "name": "Café", "labels": ["food"] },
            ],
        }))
        .unwrap();
        assert_eq!(
            listed,
            vec![
                DisplayChoice {
                    id: "bod_lobby".into(),
                    title: "Lobby".into()
                },
                DisplayChoice {
                    id: "bod_cafe".into(),
                    title: "Café".into()
                },
            ]
        );
        // Another answer is not a list, and says so rather than listing nothing.
        assert!(choices_project(serde_json::json!({ "kind": "showing", "screens": [] })).is_err());
    }

    fn entry(source: MediaSource) -> SignageMedia {
        SignageMedia {
            id: replica::body::BodyId::from_bytes([5; 16]).render(),
            name: "Ribbon cutting".into(),
            source,
            duration_ms: None,
            width: None,
            height: None,
            catalog: None,
        }
    }

    fn card() -> SignageMedia {
        entry(MediaSource::Card {
            title: "Welcome".into(),
            body: "Open house at 6".into(),
            background: "102030".into(),
            foreground: "ffffff".into(),
        })
    }

    #[test]
    fn authored_slide_renders_to_a_real_png() {
        let card = card();
        let RenderedScene::Frame(frame) = scene(Some(&card), 640, 360, 0).unwrap().scene else {
            panic!("an authored card is rendered here, not fetched");
        };
        assert_eq!(frame.media_type, FrameMediaType::Png);
        assert_eq!(frame.bytes.get(..8), Some(b"\x89PNG\r\n\x1a\n".as_slice()));
        assert!(frame.bytes.len() > 1_000);
        assert_eq!(
            spoken_summary(Some(&card), 0).as_deref(),
            Some("Welcome. Open house at 6")
        );
    }

    /// A content id in the only shape the upload route writes.
    const REAL_CONTENT_ID: &str =
        "7f3a7f3a7f3a7f3a7f3a7f3a7f3a7f3a7f3a7f3a7f3a7f3a7f3a7f3a7f3a7f3a";

    #[test]
    fn a_stored_entry_names_its_content_and_is_not_live() {
        let stored = entry(MediaSource::Stored {
            content: REAL_CONTENT_ID.into(),
            size: 4_096,
            mime: "video/mp4".into(),
        });
        assert!(stored.validate(), "a real id is admissible");
        let RenderedScene::Media(rendered) = scene(Some(&stored), 640, 360, 0).unwrap().scene
        else {
            panic!("durable bytes are fetched, not rasterised");
        };
        let MediaOrigin::Stored(content) = rendered.origin else {
            panic!("a library entry names the content plane, not a rendition");
        };
        assert_eq!(
            Some(content),
            stored.source.content_ref(),
            "the content id, never the entry that describes it"
        );
        assert_eq!(
            spoken_summary(Some(&stored), 0).as_deref(),
            Some("Ribbon cutting")
        );
    }

    /// The two namespaces cannot be confused for each other, because neither is
    /// a string by the time a surface sees it.
    #[test]
    fn a_rendition_name_is_not_a_content_id() {
        let masquerading = entry(MediaSource::Live {
            resource: REAL_CONTENT_ID.into(),
        });
        let RenderedScene::Media(rendered) = scene(Some(&masquerading), 640, 360, 0).unwrap().scene
        else {
            panic!("a live rendition is media whatever it is called");
        };
        assert!(
            matches!(rendered.origin, MediaOrigin::Live(_)),
            "a live entry names a rendition even when the name looks like a content id"
        );
    }

    #[test]
    fn a_live_entry_is_still_marked_live() {
        let live = entry(MediaSource::Live {
            resource: "lobby-cam".into(),
        });
        let RenderedScene::Media(rendered) = scene(Some(&live), 640, 360, 0).unwrap().scene else {
            panic!("a live rendition is media");
        };
        let MediaOrigin::Live(rendition) = &rendered.origin else {
            panic!("a live entry names a rendition on the live plane");
        };
        assert_eq!(rendition.as_str(), "lobby-cam");
        assert_eq!(rendered.protocol, MediaProtocol::Hls);
    }

    /// Both absences are the same fact to a screen: this renderer cannot draw
    /// the entry. Dropping the item instead would shorten the program silently.
    #[test]
    fn an_integration_and_a_dangling_item_both_blank_rather_than_vanish() {
        let integration = entry(MediaSource::Kind {
            kind: "weather".into(),
            preset: None,
            settings: [("units".to_owned(), "metric".to_owned())].into(),
        });
        for absent in [Some(&integration), None] {
            assert!(matches!(
                scene(absent, 640, 360, 0).unwrap().scene,
                RenderedScene::Blank(BlankReason::Unsupported)
            ));
        }
        assert_eq!(spoken_summary(None, 0), None);
        assert_eq!(
            spoken_summary(Some(&integration), 0).as_deref(),
            Some("Ribbon cutting"),
            "an app draws it, and the library still named it"
        );
    }

    /// Admitted and renderable are the same set.
    ///
    /// This used to bind two byte-length constants together, because both
    /// namespaces went through one string type and the World accepted any
    /// lowercase token up to 96 bytes. Such an entry validated, replicated, and
    /// then rendered as a blank forever — nothing that could decode it ever saw
    /// it. The World now admits exactly what a reference can be built from.
    #[test]
    fn a_content_id_the_world_accepts_is_always_expressible_to_a_surface() {
        let admitted = entry(MediaSource::Stored {
            content: REAL_CONTENT_ID.into(),
            size: 1,
            mime: "video/mp4".into(),
        });
        assert!(admitted.validate(), "an admissible id");
        let RenderedScene::Media(rendered) = scene(Some(&admitted), 640, 360, 0).unwrap().scene
        else {
            panic!("an admissible id renders rather than blanking");
        };
        assert!(matches!(rendered.origin, MediaOrigin::Stored(_)));

        for refused in [
            "cnt_7f3a",                      // the shape the fixtures used to carry
            &"a".repeat(63),                 // one nibble short
            &"a".repeat(65),                 // one over
            &format!("{}g", "a".repeat(63)), // right length, not hex
            &REAL_CONTENT_ID.to_uppercase(), // hex, wrong case
        ] {
            let entry = entry(MediaSource::Stored {
                content: (*refused).into(),
                size: 1,
                mime: "video/mp4".into(),
            });
            assert!(
                !entry.validate(),
                "{refused} is not an id any read could resolve"
            );
        }
    }

    #[test]
    fn athan_with_a_location_is_a_schedule_card() {
        let athan = entry(MediaSource::Kind {
            kind: "athan".into(),
            preset: None,
            settings: [
                ("latitude".into(), "51.5074".into()),
                ("longitude".into(), "-0.1278".into()),
                ("method".into(), "isna".into()),
                ("timezone".into(), "Europe/London".into()),
            ]
            .into(),
        });
        let now = 1_704_110_400_000;
        let drawn = scene(Some(&athan), 640, 360, now).unwrap();
        let RenderedScene::Frame(frame) = drawn.scene else {
            panic!("athan is rasterised here");
        };
        assert_eq!(frame.media_type, FrameMediaType::Png);
        assert_eq!(frame.bytes.get(..8), Some(b"\x89PNG\r\n\x1a\n".as_slice()));
        assert!(drawn.refresh_unix_ms.is_some());
        let spoken = spoken_summary(Some(&athan), now).expect("spoken");
        assert!(spoken.starts_with("Prayer times."), "{spoken}");
    }

    #[test]
    fn emerald_theme_paints_the_emerald_ground() {
        let athan = entry(MediaSource::Kind {
            kind: "athan".into(),
            preset: None,
            settings: [
                ("latitude".into(), "51.5074".into()),
                ("longitude".into(), "-0.1278".into()),
                ("method".into(), "isna".into()),
                ("timezone".into(), "Europe/London".into()),
                ("theme".into(), "emerald".into()),
            ]
            .into(),
        });
        let RenderedScene::Frame(frame) = scene(Some(&athan), 320, 180, 1_704_110_400_000)
            .unwrap()
            .scene
        else {
            panic!("athan is rasterised here");
        };
        let png = image::load_from_memory(&frame.bytes).unwrap().to_rgba8();
        assert_eq!(*png.get_pixel(0, 0), Rgba([12, 36, 28, 255]));
    }

    /// Three layers, narrowest last. A preset supplies the look, the entry
    /// supplies its own, and the screen's facts and place override both —
    /// which is what makes one clip correct at two venues.
    #[test]
    fn the_screen_overrides_the_preset_and_supplies_the_place() {
        let preset = signage::contract::SignagePreset {
            id: replica::body::BodyId::from_bytes([9; 16]).render(),
            kind: "athan".into(),
            name: "House style".into(),
            settings: [
                ("theme".into(), "emerald".into()),
                ("method".into(), "isna".into()),
            ]
            .into(),
        };
        let snapshot = entry(MediaSource::Kind {
            kind: "athan".into(),
            preset: Some(preset.id.clone()),
            settings: BTreeMap::new(),
        });
        let screen = signage::SignageScreen {
            id: replica::body::BodyId::from_bytes([4; 16]).render(),
            name: "Prayer hall".into(),
            place: Some(signage::Place {
                latitude: 51.5074,
                longitude: -0.1278,
                timezone: "Europe/London".into(),
                region: None,
            }),
            // This mosque reckons differently from the house preset.
            facts: [(
                "athan".to_string(),
                BTreeMap::from([("method".to_string(), "makkah".to_string())]),
            )]
            .into(),
            sync: None,
            labels: Vec::new(),
            tuned: None,
        };

        let live = resolved_entry(Some(&snapshot), &screen, &[preset]).expect("resolved");
        let MediaSource::Kind { settings, .. } = &live.source else {
            panic!("athan stays a kind");
        };
        assert_eq!(
            settings.get("theme").map(String::as_str),
            Some("emerald"),
            "the preset supplies the look"
        );
        assert_eq!(
            settings.get("method").map(String::as_str),
            Some("makkah"),
            "the venue's practice outranks the preset's"
        );
        assert_eq!(
            settings.get("timezone").map(String::as_str),
            Some("Europe/London"),
            "geography comes from the panel, not the clip"
        );

        let RenderedScene::Frame(frame) = scene(Some(&live), 320, 180, 1_704_110_400_000)
            .unwrap()
            .scene
        else {
            panic!("a sited athan entry rasterises");
        };
        let png = image::load_from_memory(&frame.bytes).unwrap().to_rgba8();
        assert_eq!(*png.get_pixel(0, 0), Rgba([12, 36, 28, 255]));
    }

    /// Aberdeen in June: isha's angle never arrives. The card used to blank
    /// for weeks with nothing saying why.
    #[test]
    fn a_high_latitude_summer_still_has_a_timetable() {
        let mut settings = BTreeMap::new();
        settings.insert("latitude".to_string(), "57.1497".to_string());
        settings.insert("longitude".to_string(), "-2.0943".to_string());
        settings.insert("timezone".to_string(), "Europe/London".to_string());
        settings.insert("method".to_string(), "isna".to_string());
        // 2024-06-21 12:00 UTC
        let day = crate::athan::times_from_settings(&settings, 1_718_971_200_000)
            .expect("a rule fills what the angle never reached");
        assert!(
            day.prayers.iter().any(|row| row.name == "Isha"),
            "isha is present under the default middle-of-night rule"
        );

        settings.insert("high_lat_rule".to_string(), "none".to_string());
        assert!(
            crate::athan::times_from_settings(&settings, 1_718_971_200_000).is_none(),
            "opting out is still allowed, and still means no card"
        );
    }
}
