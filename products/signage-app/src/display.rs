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
    BlankReason, CanonicalDisplayInput, DisplayAssessment, DisplayOutputKind, DisplayProjection,
    DisplayRenderer, DisplayRequest, DisplayResourceId, DisplaySurface, DisplaySurfaceDescriptor,
    DisplaySurfaceId, FrameMediaType, MediaOrigin, MediaProtocol, ProgramCycle, RenderedFrame,
    RenderedMedia, RenderedProgram, RenderedProgramItem, RenderedScene,
};
use world_interface::{ClientAccess, ClientInvocation, Failure};

const SURFACE_ID: &str = "signage.program";
const MAX_RENDER_WIDTH: u32 = 4_096;
const MAX_RENDER_HEIGHT: u32 = 2_160;

#[derive(Debug, Serialize, Deserialize)]
struct ProgramInput {
    program: String,
}

pub fn program_surface() -> Result<DisplaySurface, Failure> {
    let world = signage::contract::world_id();
    let mut input_digest = Sha256::new();
    input_digest.update(b"signage.program.input.v1:{program:body-id}");
    let mut renderer_identity = Sha256::new();
    renderer_identity.update(
        b"signage.program.renderer.v5:font8x8:png:library:content:lait-live:rolling-windows",
    );
    let mut descriptor = DisplaySurfaceDescriptor {
        id: DisplaySurfaceId::new(SURFACE_ID)?,
        title: "Signage program".into(),
        runtime_implementation: crate::implementation_id(),
        contract_version: 3,
        input_contract_digest: input_digest.finalize().into(),
        renderer_identity: renderer_identity.finalize().into(),
        contract_digest: [0; 32],
        outputs: BTreeSet::from([DisplayOutputKind::Frame, DisplayOutputKind::Media]),
    };
    descriptor.contract_digest = descriptor.expected_contract_digest(&world);
    Ok(DisplaySurface {
        descriptor,
        canonicalize_input,
        prepare,
        renderer: Arc::new(SignageRenderer),
    })
}

fn canonicalize_input(value: Value) -> Result<CanonicalDisplayInput, Failure> {
    let input: ProgramInput = serde_json::from_value(value)
        .map_err(|error| Failure::new(format!("invalid Signage display input: {error}")))?;
    if replica::body::BodyId::parse(&input.program).is_none() {
        return Err(Failure::new("invalid Signage program id"));
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
    let input: ProgramInput = serde_json::from_slice(request.input.as_bytes())
        .map_err(|error| Failure::new(format!("decode Signage display input: {error}")))?;
    let call = crate::encode_call(&crate::SignageRequest::ProgramGet {
        program: input.program,
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
            let crate::SignageResponse::Program {
                program: Some(program),
                media,
            } = response
            else {
                return Err(Failure::new("Signage program is unavailable"));
            };
            if !program.validate() {
                return Err(Failure::new("Signage program failed validation"));
            }
            let now_unix_ms = request
                .window_start_unix
                .checked_mul(1_000)
                .ok_or_else(|| Failure::new("Signage schedule time overflowed"))?;
            let scheduled = program
                .scheduled_at(now_unix_ms)
                .map_err(|error| Failure::new(format!("evaluate Signage schedule: {error}")))?;
            let refresh_after_ms = scheduled.next_boundary_unix_ms.and_then(|boundary| {
                let delay = boundary.saturating_sub(now_unix_ms).max(1);
                u32::try_from(delay)
                    .ok()
                    .filter(|delay| *delay <= request.window_horizon_ms)
            });
            let library: BTreeMap<&str, &SignageMedia> = media
                .iter()
                .map(|entry| (entry.id.as_str(), entry))
                .collect();
            let idle = scheduled.items.is_empty();
            let mut items = Vec::with_capacity(scheduled.items.len().max(1));
            for item in scheduled.items {
                let entry = library.get(item.media.as_str()).copied();
                items.push(RenderedProgramItem {
                    id: item.id.clone(),
                    duration_ms: item
                        .duration_ms
                        .or_else(|| entry.and_then(|entry| entry.duration_ms)),
                    scene: scene(entry, request.width, request.height)?,
                    assessment: DisplayAssessment::Current,
                    spoken_summary: spoken_summary(entry),
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
            Ok(DisplayProjection {
                program: RenderedProgram {
                    items,
                    cycle: if idle {
                        ProgramCycle::HoldLast
                    } else {
                        cycle(program.cycle)
                    },
                    refresh_after_ms,
                },
                assessment: DisplayAssessment::Current,
                spoken_summary: Some(program.name),
            })
        })
    }
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
/// nothing, and an integration, whose renderer lives in the app that owns the
/// kind rather than here.
fn scene(entry: Option<&SignageMedia>, width: u32, height: u32) -> Result<RenderedScene, Failure> {
    let Some(entry) = entry else {
        return Ok(RenderedScene::Blank(BlankReason::Unsupported));
    };
    match &entry.source {
        MediaSource::Card {
            title,
            body,
            background,
            foreground,
        } => Ok(RenderedScene::Frame(RenderedFrame {
            media_type: FrameMediaType::Png,
            width,
            height,
            bytes: render_card(title, body, background, foreground, width, height)?,
        })),
        MediaSource::Stored { .. } => Ok(entry
            .source
            .content_ref()
            .map_or(RenderedScene::Blank(BlankReason::Unsupported), |content| {
                media_scene(MediaOrigin::Stored(content))
            })),
        MediaSource::Live { resource } => Ok(DisplayResourceId::new(resource)
            .map_or(RenderedScene::Blank(BlankReason::Unsupported), |resource| {
                media_scene(MediaOrigin::Live(resource))
            })),
        MediaSource::Kind { .. } => Ok(RenderedScene::Blank(BlankReason::Unsupported)),
    }
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
fn spoken_summary(entry: Option<&SignageMedia>) -> Option<String> {
    let entry = entry?;
    let MediaSource::Card { title, body, .. } = &entry.source else {
        return Some(entry.name.clone());
    };
    Some(if body.trim().is_empty() {
        title.clone()
    } else {
        format!("{title}. {body}")
    })
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
    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(|error| Failure::new(format!("encode Signage frame: {error}")))?;
    Ok(cursor.into_inner())
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
        let RenderedScene::Frame(frame) = scene(Some(&card), 640, 360).unwrap() else {
            panic!("an authored card is rendered here, not fetched");
        };
        assert_eq!(frame.media_type, FrameMediaType::Png);
        assert_eq!(frame.bytes.get(..8), Some(b"\x89PNG\r\n\x1a\n".as_slice()));
        assert!(frame.bytes.len() > 1_000);
        assert_eq!(
            spoken_summary(Some(&card)).as_deref(),
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
        let RenderedScene::Media(rendered) = scene(Some(&stored), 640, 360).unwrap() else {
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
            spoken_summary(Some(&stored)).as_deref(),
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
        let RenderedScene::Media(rendered) = scene(Some(&masquerading), 640, 360).unwrap() else {
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
        let RenderedScene::Media(rendered) = scene(Some(&live), 640, 360).unwrap() else {
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
            settings: [("units".to_owned(), "metric".to_owned())].into(),
        });
        for absent in [Some(&integration), None] {
            assert!(matches!(
                scene(absent, 640, 360).unwrap(),
                RenderedScene::Blank(BlankReason::Unsupported)
            ));
        }
        assert_eq!(spoken_summary(None), None);
        assert_eq!(
            spoken_summary(Some(&integration)).as_deref(),
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
        let RenderedScene::Media(rendered) = scene(Some(&admitted), 640, 360).unwrap() else {
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
}
