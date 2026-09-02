//! Type on glass: the one face the console speaks, set with real metrics.
//!
//! A frame a receiver shows is a PNG this World rasterises, so the face and
//! the layout have to live here rather than in a browser. This module owns
//! the face — Inter, the console's own fallback family, Regular and Medium,
//! the two weights the design system allows — and the measuring a layout
//! needs: how wide a run is, how tall a line is, where a right edge falls.
//! A card is then a layout over these calls, not pixel arithmetic.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    reason = "raster coordinates are clamped to the image before any pixel is touched"
)]

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, Glyph, PxScale, ScaleFont};
use image::{Rgba, RgbaImage};

static REGULAR_BYTES: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");
static MEDIUM_BYTES: &[u8] = include_bytes!("../fonts/Inter-Medium.ttf");

/// The two weights the console uses. Nothing in chrome goes above Medium.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Medium,
}

/// How a run sits between two edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Everything about how one run of text is drawn.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub weight: Weight,
    /// Pixel size of the em square.
    pub size: f32,
    pub color: Rgba<u8>,
}

impl Style {
    pub const fn new(weight: Weight, size: f32, color: Rgba<u8>) -> Self {
        Self {
            weight,
            size,
            color,
        }
    }

    pub const fn with_color(self, color: Rgba<u8>) -> Self {
        Self { color, ..self }
    }
}

fn face(weight: Weight) -> &'static FontRef<'static> {
    static REGULAR: OnceLock<FontRef<'static>> = OnceLock::new();
    static MEDIUM: OnceLock<FontRef<'static>> = OnceLock::new();
    let (cell, bytes) = match weight {
        Weight::Regular => (&REGULAR, REGULAR_BYTES),
        Weight::Medium => (&MEDIUM, MEDIUM_BYTES),
    };
    cell.get_or_init(|| {
        FontRef::try_from_slice(bytes).expect("the bundled Inter face is a valid font")
    })
}

/// The line box a size produces: ascent above the baseline, height overall.
/// Layouts stack these, so a line of one style is always the same height
/// whatever it says.
pub fn line_metrics(style: Style) -> (f32, f32) {
    let scaled = face(style.weight).as_scaled(PxScale::from(style.size));
    let ascent = scaled.ascent();
    (ascent, ascent - scaled.descent())
}

/// Height of one line box.
pub fn line_height(style: Style) -> f32 {
    line_metrics(style).1
}

/// The advance width of a run, kerned.
pub fn measure(text: &str, style: Style) -> f32 {
    let scaled = face(style.weight).as_scaled(PxScale::from(style.size));
    let mut width = 0.0;
    let mut previous: Option<ab_glyph::GlyphId> = None;
    for character in text.chars() {
        let id = scaled.glyph_id(character);
        if let Some(previous) = previous {
            width += scaled.kern(previous, id);
        }
        width += scaled.h_advance(id);
        previous = Some(id);
    }
    width
}

/// Draw one run with its line box's top-left at `(x, y)`, blending glyph
/// coverage over what is already there.
pub fn draw(image: &mut RgbaImage, text: &str, x: f32, y: f32, style: Style) -> f32 {
    let font = face(style.weight);
    let scaled = font.as_scaled(PxScale::from(style.size));
    let baseline = y + scaled.ascent();
    let mut pen = x;
    let mut previous: Option<ab_glyph::GlyphId> = None;
    for character in text.chars() {
        let id = scaled.glyph_id(character);
        if let Some(previous) = previous {
            pen += scaled.kern(previous, id);
        }
        let glyph: Glyph = id.with_scale_and_position(PxScale::from(style.size), (pen, baseline));
        if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                let px = bounds.min.x + gx as f32;
                let py = bounds.min.y + gy as f32;
                if px < 0.0 || py < 0.0 {
                    return;
                }
                let (px, py) = (px as u32, py as u32);
                if px < image.width() && py < image.height() && coverage > 0.0 {
                    blend(image.get_pixel_mut(px, py), style.color, coverage.min(1.0));
                }
            });
        }
        pen += scaled.h_advance(id);
        previous = Some(id);
    }
    pen - x
}

/// Draw one run between two edges.
pub fn draw_aligned(
    image: &mut RgbaImage,
    text: &str,
    left: f32,
    right: f32,
    y: f32,
    style: Style,
    align: Align,
) -> f32 {
    let width = measure(text, style);
    let x = match align {
        Align::Left => left,
        Align::Center => left + ((right - left) - width) / 2.0,
        Align::Right => right - width,
    };
    draw(image, text, x.max(left), y, style)
}

/// Break a run into lines that fit `width`, at most `max_lines` of them; a
/// word too long for a line is cut rather than allowed to overflow.
pub fn wrap(text: &str, width: f32, style: Style, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if measure(&candidate, style) <= width || line.is_empty() {
            line = candidate;
            while measure(&line, style) > width && line.chars().count() > 1 {
                line.pop();
            }
        } else {
            lines.push(std::mem::take(&mut line));
            if lines.len() == max_lines {
                return lines;
            }
            line = word.to_string();
            while measure(&line, style) > width && line.chars().count() > 1 {
                line.pop();
            }
        }
    }
    if !line.is_empty() && lines.len() < max_lines {
        lines.push(line);
    }
    lines
}

/// Draw a wrapped paragraph; returns the height it used.
pub fn draw_paragraph(
    image: &mut RgbaImage,
    text: &str,
    left: f32,
    right: f32,
    y: f32,
    style: Style,
    align: Align,
    max_lines: usize,
    leading: f32,
) -> f32 {
    let line_height = line_height(style) * leading;
    let mut cursor = y;
    for line in wrap(text, right - left, style, max_lines) {
        draw_aligned(image, &line, left, right, cursor, style, align);
        cursor += line_height;
    }
    cursor - y
}

fn blend(under: &mut Rgba<u8>, over: Rgba<u8>, coverage: f32) {
    let alpha = coverage * f32::from(over.0[3]) / 255.0;
    for channel in 0..3 {
        let base = f32::from(under.0[channel]);
        let top = f32::from(over.0[channel]);
        under.0[channel] = (base + (top - base) * alpha).round().clamp(0.0, 255.0) as u8;
    }
    let base_alpha = f32::from(under.0[3]) / 255.0;
    under.0[3] = ((alpha + base_alpha * (1.0 - alpha)) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ink(weight: Weight, size: f32) -> Style {
        Style::new(weight, size, Rgba([255, 255, 255, 255]))
    }

    #[test]
    fn both_faces_load_and_measure_with_real_metrics() {
        let regular = ink(Weight::Regular, 40.0);
        let medium = ink(Weight::Medium, 40.0);
        let narrow = measure("Isha", regular);
        let wide = measure("Maghrib", regular);
        assert!(wide > narrow, "a longer word is wider: {wide} > {narrow}");
        assert!(measure("Athan", medium) > measure("Athan", regular) * 0.95);
        let (ascent, height) = line_metrics(regular);
        assert!(ascent > 0.0 && height > ascent);
    }

    #[test]
    fn a_run_draws_inside_its_measured_box_and_alignment_holds_the_edge() {
        let mut image = RgbaImage::from_pixel(400, 100, Rgba([0, 0, 0, 255]));
        let style = ink(Weight::Regular, 32.0);
        let width = measure("16:30", style);
        draw_aligned(&mut image, "16:30", 0.0, 400.0, 10.0, style, Align::Right);
        let lit = |x: u32| (0..100).any(|y| image.get_pixel(x, y).0[0] > 0);
        assert!(!lit(399 - width as u32 - 8), "nothing lit left of the run");
        assert!((380..400).any(lit), "the run reaches its right edge");
    }

    #[test]
    fn wrapping_never_exceeds_its_width_and_honours_the_line_cap() {
        let style = ink(Weight::Regular, 24.0);
        let lines = wrap(
            "Tune it to a channel or address a broadcast at it, then wait",
            180.0,
            style,
            3,
        );
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert!(measure(line, style) <= 180.0, "{line}");
        }
    }
}
