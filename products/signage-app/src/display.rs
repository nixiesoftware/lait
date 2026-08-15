#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "bounded raster coordinates are checked before image access"
)]

use std::collections::BTreeSet;
use std::io::Cursor;
use std::sync::Arc;

use font8x8::{UnicodeFonts, BASIC_FONTS};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use world_interface::display::{
    CanonicalDisplayInput, DisplayAssessment, DisplayOutputKind, DisplayProjection,
    DisplayRenderer, DisplayRequest, DisplaySurface, DisplaySurfaceDescriptor, DisplaySurfaceId,
    FrameMediaType, ProgramCycle, RenderedFrame, RenderedProgram, RenderedProgramItem,
    RenderedScene,
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
    renderer_identity.update(b"signage.program.frame-renderer.v1:font8x8:png");
    let mut descriptor = DisplaySurfaceDescriptor {
        id: DisplaySurfaceId::new(SURFACE_ID)?,
        title: "Signage program".into(),
        runtime_implementation: crate::implementation_id(),
        contract_version: 1,
        input_contract_digest: input_digest.finalize().into(),
        renderer_identity: renderer_identity.finalize().into(),
        contract_digest: [0; 32],
        outputs: BTreeSet::from([DisplayOutputKind::Frame]),
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
            } = response
            else {
                return Err(Failure::new("Signage program is unavailable"));
            };
            if !program.validate() {
                return Err(Failure::new("Signage program failed validation"));
            }
            let mut items = Vec::with_capacity(program.items.len());
            for item in &program.items {
                let bytes = render_item(item, request.width, request.height)?;
                items.push(RenderedProgramItem {
                    id: item.id.clone(),
                    duration_ms: item.duration_ms,
                    scene: RenderedScene::Frame(RenderedFrame {
                        media_type: FrameMediaType::Png,
                        width: request.width,
                        height: request.height,
                        bytes,
                    }),
                    assessment: DisplayAssessment::Current,
                    spoken_summary: Some(spoken_summary(item)),
                });
            }
            Ok(DisplayProjection {
                program: RenderedProgram {
                    items,
                    cycle: cycle(program.cycle),
                    refresh_after_ms: None,
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

fn spoken_summary(item: &signage::SignageItem) -> String {
    if item.body.trim().is_empty() {
        item.title.clone()
    } else {
        format!("{}. {}", item.title, item.body)
    }
}

fn render_item(item: &signage::SignageItem, width: u32, height: u32) -> Result<Vec<u8>, Failure> {
    let background = rgb(&item.background)?;
    let foreground = rgb(&item.foreground)?;
    let mut image = RgbaImage::from_pixel(width, height, background);
    let inset = width.min(height) / 12;
    let title_scale = (width / 180).min(height / 80).clamp(2, 12);
    let body_scale = title_scale.saturating_sub(2).max(2);
    let title_y = height / 4;
    draw_wrapped(
        &mut image,
        &item.title,
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
        &item.body,
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

    #[test]
    fn authored_slide_renders_to_a_real_png() {
        let item = signage::SignageItem {
            id: "welcome".into(),
            title: "Welcome".into(),
            body: "Open house at 6".into(),
            background: "102030".into(),
            foreground: "ffffff".into(),
            duration_ms: Some(10_000),
        };
        let png = render_item(&item, 640, 360).unwrap();
        assert_eq!(png.get(..8), Some(b"\x89PNG\r\n\x1a\n".as_slice()));
        assert!(png.len() > 1_000);
    }
}
