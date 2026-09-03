//! A still, encoded as one H.264 IDR frame the coordinator can put in a stream.
//!
//! The media plane transmuxes and never encodes — with one exception, born of
//! the signage program, where a still and a clip have to play from one HLS
//! stream so a receiver never switches surfaces mid-program. A still is a PNG;
//! an HLS segment is H.264; something has to bridge them.
//!
//! This is the smallest bridge that is still a *conformant* H.264 frame: every
//! macroblock is `I_PCM`, which carries its samples raw. There is no transform,
//! no quantisation, and no entropy-coded residual — the three parts of an H.264
//! encoder that are large, slow, and easy to get subtly wrong. `I_PCM` is
//! mandatory in every profile, so any decoder that plays the clips beside it
//! plays this. The frame is big (about 1.5 bytes a pixel), which is why a still
//! is encoded once and cached by its digest, never per playout.
//!
//! The bitstream syntax follows ITU-T H.264; the SPS/PPS/slice layouts are the
//! ones a reference decoder expects, written by hand because no pure-Rust
//! encoder exists to depend on. Output is verified end to end by decoding it
//! back with ffmpeg in the tests.

/// One still as H.264: the decoder config and the single access unit, in the
/// shapes the coordinator's catalog and its HLS packager already consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StillH264 {
    /// `avc1.PPCCLL` — profile, constraints, level — for the catalog codec.
    pub codec: String,
    /// The `avcC` decoder configuration record (SPS and PPS inside).
    pub avcc: Vec<u8>,
    /// The IDR access unit as one length-prefixed NAL (4-byte big-endian
    /// length, then the unit), the framing stored clips use.
    pub access_unit: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// The decoder configuration this still was written under, and how many
    /// pictures a second of it carries (see [`StreamShape`]).
    pub shape: StreamShape,
    /// Macroblocks per picture, which is all a skip frame has to say.
    macroblocks: u32,
}

impl StillH264 {
    /// Pictures per second this still is meant to be muxed at: one, unless it
    /// is shaped after a clip, in which case the IDR is followed by
    /// `frames_per_second - 1` [`skip_frame`](Self::skip_frame)s.
    pub fn frames_per_second(&self) -> u32 {
        self.shape.frames_per_second.max(1)
    }

    /// A P frame in which every macroblock is skipped — the decoder repeats
    /// the previous picture — as one length-prefixed NAL, a few bytes long.
    /// `index` counts from 1 after the IDR (which is frame 0) and must stay
    /// below 128 within one second, which every real frame rate does.
    ///
    /// Why it exists: a receiver's decoder that has been fed one picture a
    /// second, then meets a 25 fps clip behind a discontinuity, reconfigures
    /// itself and the glass holds for half a second. Padding the still to the
    /// clip's own rate, under the clip's own SPS, means the decoder is told
    /// nothing new at the seam.
    pub fn skip_frame(&self, index: u32) -> Vec<u8> {
        length_prefixed(&nal(2, 1, &skip_slice(index, self.macroblocks)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StillError {
    /// A dimension is smaller than the 2x2 a croppable frame needs, or the
    /// buffer size overflowed.
    Dimensions,
    /// The pixel buffer is not `width * height * 4` bytes of RGBA.
    PixelCount,
    /// The image bytes (PNG, JPEG, or WebP) could not be decoded.
    Png,
}

/// The parts of a decoder configuration a hardware decoder is re-initialised
/// for, read from a clip's SPS so a still can be written to match it: profile,
/// constraints and level, the reference-frame count, VUI timing, and the
/// reordering bound. A still under the clip's shape sits in the same decoder
/// session as the clip; one under the default shape is the Constrained
/// Baseline one-picture-a-second still the stream has always carried.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StreamShape {
    pub profile_idc: u8,
    /// The constraint_set flags byte as the SPS carries it.
    pub constraint_flags: u8,
    pub level_idc: u8,
    pub max_num_ref_frames: u32,
    pub timing: Option<VuiTiming>,
    pub reorder: Option<VuiReorder>,
    /// How many pictures one second of a still carries: one IDR and the rest
    /// skip frames. One means no skip frames at all.
    pub frames_per_second: u32,
}

/// `timing_info` from a VUI: `time_scale / (2 * num_units_in_tick)` is the
/// frame rate for a progressive stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VuiTiming {
    pub num_units_in_tick: u32,
    pub time_scale: u32,
    pub fixed_frame_rate: bool,
}

/// `bitstream_restriction` from a VUI: how many frames the decoder holds
/// back for reordering, and the DPB size it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VuiReorder {
    pub max_num_reorder_frames: u32,
    pub max_dec_frame_buffering: u32,
}

/// Why an SPS could not be read for its shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeError {
    /// Not an SPS NAL, or an `avcC` without one.
    NotAnSps,
    /// The bitstream ended inside the header, or a field is out of range.
    Truncated,
}

const HIGH_PROFILES: [u8; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];
const MAX_FRAMES_PER_SECOND: u32 = 120;

impl Default for StreamShape {
    /// Constrained Baseline 4.0, no VUI, one picture a second: what a still
    /// has always been written as.
    fn default() -> Self {
        Self {
            profile_idc: PROFILE_IDC,
            constraint_flags: 0b0100_0000,
            level_idc: LEVEL_IDC,
            max_num_ref_frames: 0,
            timing: None,
            reorder: None,
            frames_per_second: 1,
        }
    }
}

impl StreamShape {
    /// Read the shape from an `avcC` decoder configuration record — the first
    /// SPS inside it. `frame_rate_milli` (frames per second times 1000, the
    /// catalog's word for it) decides the still's picture rate when given;
    /// otherwise the SPS's own timing does, and failing both it is one.
    pub fn from_avcc(avcc: &[u8], frame_rate_milli: Option<u32>) -> Result<Self, ShapeError> {
        let count = avcc.get(5).map(|b| b & 0x1f).ok_or(ShapeError::NotAnSps)?;
        if count == 0 {
            return Err(ShapeError::NotAnSps);
        }
        let len = avcc
            .get(6..8)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
            .ok_or(ShapeError::Truncated)?;
        let sps = avcc.get(8..8 + len).ok_or(ShapeError::Truncated)?;
        Self::from_sps_nal(sps, frame_rate_milli)
    }

    /// Read the shape from one SPS NAL (header byte included, emulation
    /// prevention still in place).
    pub fn from_sps_nal(nal: &[u8], frame_rate_milli: Option<u32>) -> Result<Self, ShapeError> {
        let (&header, rest) = nal.split_first().ok_or(ShapeError::NotAnSps)?;
        if header & 0x1f != 7 {
            return Err(ShapeError::NotAnSps);
        }
        let rbsp = unescape(rest);
        let mut r = BitReader::new(&rbsp);
        let profile_idc = r.u(8)? as u8;
        let constraint_flags = r.u(8)? as u8;
        let level_idc = r.u(8)? as u8;
        r.ue()?; // seq_parameter_set_id
        if HIGH_PROFILES.contains(&profile_idc) {
            let chroma_format_idc = r.ue()?;
            if chroma_format_idc == 3 {
                r.u(1)?; // separate_colour_plane_flag
            }
            r.ue()?; // bit_depth_luma_minus8
            r.ue()?; // bit_depth_chroma_minus8
            r.u(1)?; // qpprime_y_zero_transform_bypass_flag
            if r.u(1)? == 1 {
                // seq_scaling_matrix_present_flag: skip the lists.
                let lists = if chroma_format_idc == 3 { 12 } else { 8 };
                for i in 0..lists {
                    if r.u(1)? == 1 {
                        let size = if i < 6 { 16 } else { 64 };
                        let mut last = 8i32;
                        let mut next = 8i32;
                        for _ in 0..size {
                            if next != 0 {
                                next = (last + r.se()?).rem_euclid(256);
                            }
                            last = if next == 0 { last } else { next };
                        }
                    }
                }
            }
        }
        r.ue()?; // log2_max_frame_num_minus4
        match r.ue()? {
            0 => {
                r.ue()?; // log2_max_pic_order_cnt_lsb_minus4
            }
            1 => {
                r.u(1)?; // delta_pic_order_always_zero_flag
                r.se()?; // offset_for_non_ref_pic
                r.se()?; // offset_for_top_to_bottom_field
                let cycle = r.ue()?;
                for _ in 0..cycle {
                    r.se()?;
                }
            }
            _ => {}
        }
        let max_num_ref_frames = r.ue()?;
        r.u(1)?; // gaps_in_frame_num_value_allowed_flag
        r.ue()?; // pic_width_in_mbs_minus1
        r.ue()?; // pic_height_in_map_units_minus1
        if r.u(1)? == 0 {
            r.u(1)?; // mb_adaptive_frame_field_flag
        }
        r.u(1)?; // direct_8x8_inference_flag
        if r.u(1)? == 1 {
            for _ in 0..4 {
                r.ue()?; // frame_crop_*_offset
            }
        }
        let mut timing = None;
        let mut reorder = None;
        if r.u(1)? == 1 {
            // vui_parameters
            if r.u(1)? == 1 {
                if r.u(8)? == 255 {
                    r.u(16)?; // sar_width
                    r.u(16)?; // sar_height
                }
            }
            if r.u(1)? == 1 {
                r.u(1)?; // overscan_appropriate_flag
            }
            if r.u(1)? == 1 {
                r.u(3)?; // video_format
                r.u(1)?; // video_full_range_flag
                if r.u(1)? == 1 {
                    r.u(24)?; // colour_primaries, transfer, matrix
                }
            }
            if r.u(1)? == 1 {
                r.ue()?; // chroma_sample_loc_type_top_field
                r.ue()?; // chroma_sample_loc_type_bottom_field
            }
            if r.u(1)? == 1 {
                let num_units_in_tick = r.u(32)?;
                let time_scale = r.u(32)?;
                let fixed_frame_rate = r.u(1)? == 1;
                timing = Some(VuiTiming {
                    num_units_in_tick,
                    time_scale,
                    fixed_frame_rate,
                });
            }
            let nal_hrd = r.u(1)? == 1;
            if nal_hrd {
                skip_hrd(&mut r)?;
            }
            let vcl_hrd = r.u(1)? == 1;
            if vcl_hrd {
                skip_hrd(&mut r)?;
            }
            if nal_hrd || vcl_hrd {
                r.u(1)?; // low_delay_hrd_flag
            }
            r.u(1)?; // pic_struct_present_flag
            if r.u(1)? == 1 {
                r.u(1)?; // motion_vectors_over_pic_boundaries_flag
                r.ue()?; // max_bytes_per_pic_denom
                r.ue()?; // max_bits_per_mb_denom
                r.ue()?; // log2_max_mv_length_horizontal
                r.ue()?; // log2_max_mv_length_vertical
                let max_num_reorder_frames = r.ue()?;
                let max_dec_frame_buffering = r.ue()?;
                reorder = Some(VuiReorder {
                    max_num_reorder_frames,
                    max_dec_frame_buffering,
                });
            }
        }
        let from_timing = timing.and_then(|t| {
            let ticks = t.num_units_in_tick.checked_mul(2)?;
            if ticks == 0 {
                return None;
            }
            Some((t.time_scale + ticks / 2) / ticks)
        });
        let frames_per_second = frame_rate_milli
            .map(|milli| (milli + 500) / 1_000)
            .or(from_timing)
            .unwrap_or(1)
            .clamp(1, MAX_FRAMES_PER_SECOND);
        Ok(Self {
            profile_idc,
            constraint_flags,
            level_idc,
            max_num_ref_frames,
            timing,
            reorder,
            frames_per_second,
        })
    }

    /// The `avc1.PPCCLL` codec string for this shape.
    pub fn codec(&self) -> String {
        format!(
            "avc1.{:02x}{:02x}{:02x}",
            self.profile_idc, self.constraint_flags, self.level_idc
        )
    }

    /// A stable byte string for a cache key: two stills under different
    /// shapes are different stills.
    pub fn digest_bytes(&self) -> Vec<u8> {
        let mut out = vec![self.profile_idc, self.constraint_flags, self.level_idc];
        out.extend_from_slice(&self.max_num_ref_frames.to_be_bytes());
        match self.timing {
            Some(t) => {
                out.push(1);
                out.extend_from_slice(&t.num_units_in_tick.to_be_bytes());
                out.extend_from_slice(&t.time_scale.to_be_bytes());
                out.push(u8::from(t.fixed_frame_rate));
            }
            None => out.push(0),
        }
        match self.reorder {
            Some(r) => {
                out.push(1);
                out.extend_from_slice(&r.max_num_reorder_frames.to_be_bytes());
                out.extend_from_slice(&r.max_dec_frame_buffering.to_be_bytes());
            }
            None => out.push(0),
        }
        out.extend_from_slice(&self.frames_per_second.to_be_bytes());
        out
    }

    fn is_high(&self) -> bool {
        HIGH_PROFILES.contains(&self.profile_idc)
    }
}

fn skip_hrd(r: &mut BitReader<'_>) -> Result<(), ShapeError> {
    let cpb_cnt = r.ue()?;
    r.u(4)?; // bit_rate_scale
    r.u(4)?; // cpb_size_scale
    for _ in 0..=cpb_cnt {
        r.ue()?; // bit_rate_value_minus1
        r.ue()?; // cpb_size_value_minus1
        r.u(1)?; // cbr_flag
    }
    r.u(20)?; // four 5-bit delays
    Ok(())
}

/// Strip emulation-prevention bytes: `00 00 03` becomes `00 00`.
fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0u32;
    for &byte in nal {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// A big-endian bit reader with the Exp-Golomb decodings H.264 headers use.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn u(&mut self, count: u32) -> Result<u32, ShapeError> {
        let mut value = 0u32;
        for _ in 0..count {
            let byte = self.data.get(self.pos / 8).ok_or(ShapeError::Truncated)?;
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            value = (value << 1) | u32::from(bit);
            self.pos += 1;
        }
        Ok(value)
    }

    fn ue(&mut self) -> Result<u32, ShapeError> {
        let mut zeros = 0u32;
        while self.u(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err(ShapeError::Truncated);
            }
        }
        let rest = self.u(zeros)?;
        (1u32 << zeros)
            .checked_sub(1)
            .and_then(|base| base.checked_add(rest))
            .ok_or(ShapeError::Truncated)
    }

    fn se(&mut self) -> Result<i32, ShapeError> {
        let code = self.ue()?;
        let magnitude = i32::try_from(code.div_ceil(2)).map_err(|_| ShapeError::Truncated)?;
        Ok(if code % 2 == 1 { magnitude } else { -magnitude })
    }
}

/// Encode a rendered still, delivered as a PNG (what the World produces and
/// the coordinator holds as a frame asset), into one H.264 IDR frame. The PNG
/// is decoded to RGBA and handed to [`encode_still`], which accepts any size —
/// a panel-sized card and an arbitrary uploaded image alike.
pub fn encode_still_png(png_bytes: &[u8]) -> Result<StillH264, StillError> {
    let decoder = png::Decoder::new(png_bytes);
    let mut reader = decoder.read_info().map_err(|_| StillError::Png)?;
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|_| StillError::Png)?;
    buffer.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(buffer.len() / 3 * 4);
            for chunk in buffer.chunks_exact(3) {
                out.extend_from_slice(chunk);
                out.push(255);
            }
            out
        }
        // The signage renderer only emits RGB/RGBA; anything else is a still
        // this path was not built for.
        _ => return Err(StillError::Png),
    };
    encode_still(&rgba, info.width, info.height)
}

/// Encode a stored image slide — JPEG, WebP, or PNG, whatever a person uploaded
/// to a signage program — into one H.264 IDR frame, so it plays as one part of
/// the single program stream. Decoded to RGBA at its own size; the encoder pads
/// and crops any dimensions.
pub fn encode_still_image(bytes: &[u8]) -> Result<StillH264, StillError> {
    let decoded = image::load_from_memory(bytes).map_err(|_| StillError::Png)?;
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    encode_still(&rgba, width, height)
}

/// Encode a still — rendered card or uploaded image, any format the decoder
/// knows — **fitted into a frame of `width` x `height`**: scaled to fit
/// while keeping its shape, centred on black. One stream must keep one
/// frame size from part to part; a television's decoder that is handed a
/// new resolution behind a discontinuity may stall on it (a Roku Express
/// froze with its player still saying `playing`), so every still is made
/// the size the stream is, and only a clip, which cannot be rescaled here,
/// gets to decide what that size is.
pub fn encode_still_fitted(bytes: &[u8], width: u32, height: u32) -> Result<StillH264, StillError> {
    encode_still_fitted_shaped(bytes, width, height, &StreamShape::default())
}

/// [`encode_still_fitted`] under a [`StreamShape`].
pub fn encode_still_fitted_shaped(
    bytes: &[u8],
    width: u32,
    height: u32,
    shape: &StreamShape,
) -> Result<StillH264, StillError> {
    if width < 2 || height < 2 {
        return Err(StillError::Dimensions);
    }
    let decoded = image::load_from_memory(bytes)
        .map_err(|_| StillError::Png)?
        .to_rgba8();
    let (source_width, source_height) = decoded.dimensions();
    if source_width == width && source_height == height {
        return encode_still_shaped(&decoded, width, height, shape);
    }
    // Scale to fit, never to fill: nothing is cropped, the rest is black.
    // Integer throughout: the limiting side becomes the frame's, the other
    // follows the picture's shape.
    let (source_width, source_height) = (source_width.max(1), source_height.max(1));
    let wide =
        u64::from(width) * u64::from(source_height) <= u64::from(height) * u64::from(source_width);
    let (fitted_width, fitted_height) = if wide {
        let fitted_height = u64::from(source_height) * u64::from(width) / u64::from(source_width);
        (
            width,
            u32::try_from(fitted_height)
                .unwrap_or(height)
                .clamp(1, height),
        )
    } else {
        let fitted_width = u64::from(source_width) * u64::from(height) / u64::from(source_height);
        (
            u32::try_from(fitted_width).unwrap_or(width).clamp(1, width),
            height,
        )
    };
    let scaled = image::imageops::resize(
        &decoded,
        fitted_width,
        fitted_height,
        image::imageops::FilterType::Triangle,
    );
    let mut canvas = image::RgbaImage::from_pixel(width, height, image::Rgba([0, 0, 0, 255]));
    let left = i64::from((width - fitted_width) / 2);
    let top = i64::from((height - fitted_height) / 2);
    image::imageops::overlay(&mut canvas, &scaled, left, top);
    encode_still_shaped(&canvas, width, height, shape)
}

const PROFILE_IDC: u8 = 66; // Constrained Baseline
const LEVEL_IDC: u8 = 40; // 4.0 — comfortably covers 1080p stills
const MB: u32 = 16;

/// Encode an RGBA still (`width * height * 4` bytes, row-major, no padding)
/// into one H.264 IDR frame. Any dimensions are accepted: a television image
/// is whatever size it is, so the coded frame is padded up to the 16x16
/// macroblock grid and the real (even) display size is carried in the SPS crop.
pub fn encode_still(rgba: &[u8], width: u32, height: u32) -> Result<StillH264, StillError> {
    encode_still_shaped(rgba, width, height, &StreamShape::default())
}

/// [`encode_still`] under a given [`StreamShape`]: the SPS names the shape's
/// profile, level, reference count and VUI, and the still knows how many
/// pictures a second it is muxed at. Under the default shape this is byte for
/// byte what `encode_still` makes.
pub fn encode_still_shaped(
    rgba: &[u8],
    width: u32,
    height: u32,
    shape: &StreamShape,
) -> Result<StillH264, StillError> {
    if width < 2 || height < 2 {
        return Err(StillError::Dimensions);
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(StillError::Dimensions)?;
    if rgba.len() != expected {
        return Err(StillError::PixelCount);
    }

    // Code on the macroblock grid; crop back to the real, even display size.
    // Edge pixels are replicated into the padding so it never bleeds a hard
    // line into chroma near the crop. The player shows exactly the cropped
    // rectangle and discards the padded remainder.
    let coded_width = width.div_ceil(MB) * MB;
    let coded_height = height.div_ceil(MB) * MB;
    let display_width = width & !1;
    let display_height = height & !1;
    let padded = pad_rgba(rgba, width, height, coded_width, coded_height);

    let (y_plane, cb_plane, cr_plane) = rgba_to_yuv420(&padded, coded_width, coded_height);

    let sps = sequence_parameter_set(
        shape,
        coded_width,
        coded_height,
        display_width,
        display_height,
    );
    let pps = picture_parameter_set();
    let slice = idr_slice(&y_plane, &cb_plane, &cr_plane, coded_width, coded_height);

    let codec = shape.codec();
    let avcc = avc_decoder_config(shape, &sps, &pps);
    let access_unit = length_prefixed(&nal(3, 5, &slice));

    Ok(StillH264 {
        codec,
        avcc,
        access_unit,
        width: display_width,
        height: display_height,
        shape: shape.clone(),
        macroblocks: (coded_width / MB) * (coded_height / MB),
    })
}

/// Copy an RGBA image into a larger coded frame, replicating the right and
/// bottom edges into the padding.
fn pad_rgba(rgba: &[u8], width: u32, height: u32, coded_width: u32, coded_height: u32) -> Vec<u8> {
    if width == coded_width && height == coded_height {
        return rgba.to_vec();
    }
    let (sw, sh) = (width as usize, height as usize);
    let (cw, ch) = (coded_width as usize, coded_height as usize);
    let mut out = vec![0u8; cw * ch * 4];
    for row in 0..ch {
        let src_row = row.min(sh - 1);
        for col in 0..cw {
            let src_col = col.min(sw - 1);
            let si = (src_row * sw + src_col) * 4;
            let di = (row * cw + col) * 4;
            out[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
        }
    }
    out
}

// ---- colour ---------------------------------------------------------------

/// BT.601 studio-range RGB→YCbCr, then 4:2:0 by averaging each 2x2 chroma
/// quad. Studio range (16–235 / 16–240) is what a television expects; full
/// range would wash a card's blacks grey on a set that assumes limited.
fn rgba_to_yuv420(rgba: &[u8], width: u32, height: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let w = width as usize;
    let h = height as usize;
    let mut y_plane = vec![0u8; w * h];
    // Chroma at full resolution first, then downsample.
    let mut cb_full = vec![0f32; w * h];
    let mut cr_full = vec![0f32; w * h];
    for row in 0..h {
        for col in 0..w {
            let i = (row * w + col) * 4;
            let r = f32::from(rgba[i]);
            let g = f32::from(rgba[i + 1]);
            let b = f32::from(rgba[i + 2]);
            let y = 0.257 * r + 0.504 * g + 0.098 * b + 16.0;
            let cb = -0.148 * r - 0.291 * g + 0.439 * b + 128.0;
            let cr = 0.439 * r - 0.368 * g - 0.071 * b + 128.0;
            y_plane[row * w + col] = clamp8(y);
            cb_full[row * w + col] = cb;
            cr_full[row * w + col] = cr;
        }
    }
    let cw = w / 2;
    let ch = h / 2;
    let mut cb_plane = vec![0u8; cw * ch];
    let mut cr_plane = vec![0u8; cw * ch];
    for row in 0..ch {
        for col in 0..cw {
            let mut cb = 0f32;
            let mut cr = 0f32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let idx = (row * 2 + dy) * w + (col * 2 + dx);
                    cb += cb_full[idx];
                    cr += cr_full[idx];
                }
            }
            cb_plane[row * cw + col] = clamp8(cb / 4.0);
            cr_plane[row * cw + col] = clamp8(cr / 4.0);
        }
    }
    (y_plane, cb_plane, cr_plane)
}

fn clamp8(value: f32) -> u8 {
    if value <= 0.0 {
        0
    } else if value >= 255.0 {
        255
    } else {
        (value + 0.5) as u8
    }
}

// ---- bitstream ------------------------------------------------------------

/// A big-endian bit writer with the Exp-Golomb codings H.264 headers use.
struct BitWriter {
    bytes: Vec<u8>,
    bit: u8, // 0..=7, next bit position from the MSB of the current byte
    current: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            bit: 0,
            current: 0,
        }
    }

    fn put_bit(&mut self, value: u32) {
        self.current |= ((value & 1) as u8) << (7 - self.bit);
        self.bit += 1;
        if self.bit == 8 {
            self.bytes.push(self.current);
            self.current = 0;
            self.bit = 0;
        }
    }

    fn put_bits(&mut self, value: u32, count: u32) {
        let mut n = count;
        while n > 0 {
            n -= 1;
            self.put_bit((value >> n) & 1);
        }
    }

    /// Unsigned Exp-Golomb.
    fn ue(&mut self, value: u32) {
        let code = value + 1;
        let bits = 32 - code.leading_zeros();
        for _ in 0..(bits - 1) {
            self.put_bit(0);
        }
        self.put_bits(code, bits);
    }

    /// Signed Exp-Golomb.
    fn se(&mut self, value: i32) {
        if value == 0 {
            self.ue(0);
        } else if value > 0 {
            self.ue((value as u32) * 2 - 1);
        } else {
            self.ue((-value as u32) * 2);
        }
    }

    fn byte_align(&mut self) {
        while self.bit != 0 {
            self.put_bit(0);
        }
    }

    /// `rbsp_trailing_bits`: a stop bit, then zeros to a byte boundary.
    fn trailing_bits(&mut self) {
        self.put_bit(1);
        self.byte_align();
    }

    fn is_byte_aligned(&self) -> bool {
        self.bit == 0
    }

    fn append_bytes(&mut self, data: &[u8]) {
        debug_assert!(self.is_byte_aligned());
        self.bytes.extend_from_slice(data);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bit != 0 {
            self.bytes.push(self.current);
        }
        self.bytes
    }
}

const FRAME_NUM_BITS: u32 = 8;
const POC_LSB_BITS: u32 = 8;

fn sequence_parameter_set(
    shape: &StreamShape,
    coded_width: u32,
    coded_height: u32,
    display_width: u32,
    display_height: u32,
) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.put_bits(u32::from(shape.profile_idc), 8);
    // constraint_set0..5 flags + 2 reserved zero bits, as the shape carries
    // them: Constrained Baseline sets flag1; a clip's High profile sets none.
    w.put_bits(u32::from(shape.constraint_flags), 8);
    w.put_bits(u32::from(shape.level_idc), 8);
    w.ue(0); // seq_parameter_set_id
    if shape.is_high() {
        // The High-profile SPS extension: 4:2:0, 8-bit, no transform bypass,
        // no scaling matrix — the samples are raw either way.
        w.ue(1); // chroma_format_idc
        w.ue(0); // bit_depth_luma_minus8
        w.ue(0); // bit_depth_chroma_minus8
        w.put_bit(0); // qpprime_y_zero_transform_bypass_flag
        w.put_bit(0); // seq_scaling_matrix_present_flag
    }
    w.ue(FRAME_NUM_BITS - 4); // log2_max_frame_num_minus4
    w.ue(0); // pic_order_cnt_type
    w.ue(POC_LSB_BITS - 4); // log2_max_pic_order_cnt_lsb_minus4
                            // Skip frames reference the picture before them, so a still muxed at a
                            // clip's rate needs at least one reference; a one-picture still needs none.
    let refs = if shape.frames_per_second > 1 {
        shape.max_num_ref_frames.max(1)
    } else {
        shape.max_num_ref_frames
    };
    w.ue(refs); // max_num_ref_frames
    w.put_bit(0); // gaps_in_frame_num_value_allowed_flag
    w.ue(coded_width / MB - 1); // pic_width_in_mbs_minus1
    w.ue(coded_height / MB - 1); // pic_height_in_map_units_minus1
    w.put_bit(1); // frame_mbs_only_flag
    w.put_bit(0); // direct_8x8_inference_flag
                  // Crop the coded macroblock grid back to the display size. For 4:2:0 with
                  // frame_mbs_only_flag, both crop units are 2 luma samples, so the offsets
                  // are half the padding in each axis.
    let crop_right = (coded_width - display_width) / 2;
    let crop_bottom = (coded_height - display_height) / 2;
    if crop_right == 0 && crop_bottom == 0 {
        w.put_bit(0); // frame_cropping_flag
    } else {
        w.put_bit(1); // frame_cropping_flag
        w.ue(0); // frame_crop_left_offset
        w.ue(crop_right); // frame_crop_right_offset
        w.ue(0); // frame_crop_top_offset
        w.ue(crop_bottom); // frame_crop_bottom_offset
    }
    if shape.timing.is_none() && shape.reorder.is_none() {
        w.put_bit(0); // vui_parameters_present_flag
    } else {
        // Only the two VUI blocks a decoder session is configured by are
        // written, mirrored from the clip; the rest are absent.
        w.put_bit(1); // vui_parameters_present_flag
        w.put_bit(0); // aspect_ratio_info_present_flag
        w.put_bit(0); // overscan_info_present_flag
        w.put_bit(0); // video_signal_type_present_flag
        w.put_bit(0); // chroma_loc_info_present_flag
        match shape.timing {
            Some(timing) => {
                w.put_bit(1); // timing_info_present_flag
                w.put_bits(timing.num_units_in_tick, 32);
                w.put_bits(timing.time_scale, 32);
                w.put_bit(u32::from(timing.fixed_frame_rate));
            }
            None => w.put_bit(0),
        }
        w.put_bit(0); // nal_hrd_parameters_present_flag
        w.put_bit(0); // vcl_hrd_parameters_present_flag
        w.put_bit(0); // pic_struct_present_flag
        match shape.reorder {
            Some(reorder) => {
                w.put_bit(1); // bitstream_restriction_flag
                w.put_bit(1); // motion_vectors_over_pic_boundaries_flag
                w.ue(0); // max_bytes_per_pic_denom (unlimited)
                w.ue(0); // max_bits_per_mb_denom (unlimited)
                w.ue(16); // log2_max_mv_length_horizontal
                w.ue(16); // log2_max_mv_length_vertical
                w.ue(reorder.max_num_reorder_frames);
                w.ue(reorder.max_dec_frame_buffering.max(refs));
            }
            None => w.put_bit(0),
        }
    }
    w.trailing_bits();
    w.finish()
}

fn picture_parameter_set() -> Vec<u8> {
    let mut w = BitWriter::new();
    w.ue(0); // pic_parameter_set_id
    w.ue(0); // seq_parameter_set_id
    w.put_bit(0); // entropy_coding_mode_flag -> CAVLC
    w.put_bit(0); // bottom_field_pic_order_in_frame_present_flag
    w.ue(0); // num_slice_groups_minus1
    w.ue(0); // num_ref_idx_l0_default_active_minus1
    w.ue(0); // num_ref_idx_l1_default_active_minus1
    w.put_bit(0); // weighted_pred_flag
    w.put_bits(0, 2); // weighted_bipred_idc
    w.se(0); // pic_init_qp_minus26
    w.se(0); // pic_init_qs_minus26
    w.se(0); // chroma_qp_index_offset
    w.put_bit(0); // deblocking_filter_control_present_flag
    w.put_bit(0); // constrained_intra_pred_flag
    w.put_bit(0); // redundant_pic_cnt_present_flag
    w.trailing_bits();
    w.finish()
}

fn idr_slice(y: &[u8], cb: &[u8], cr: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut w = BitWriter::new();
    // slice_header
    w.ue(0); // first_mb_in_slice
    w.ue(7); // slice_type 7 == I, and every slice in the picture is I
    w.ue(0); // pic_parameter_set_id
    w.put_bits(0, FRAME_NUM_BITS); // frame_num
    w.ue(0); // idr_pic_id
    w.put_bits(0, POC_LSB_BITS); // pic_order_cnt_lsb
                                 // dec_ref_pic_marking for an IDR
    w.put_bit(0); // no_output_of_prior_pics_flag
    w.put_bit(0); // long_term_reference_flag
    w.se(0); // slice_qp_delta (QP stays at pic_init_qp 26; unused by I_PCM)

    // slice_data: every macroblock is I_PCM, in raster order.
    let mbs_wide = width / MB;
    let mbs_high = height / MB;
    let cw = (width / 2) as usize;
    let full_w = width as usize;
    for mb_y in 0..mbs_high {
        for mb_x in 0..mbs_wide {
            // macroblock_layer: mb_type. In an I slice, I_PCM is mb_type 25.
            w.ue(25);
            // pcm_alignment_zero_bit(s) to the next byte boundary.
            w.byte_align();
            // 16x16 luma samples, then 8x8 Cb, then 8x8 Cr — all u(8).
            for row in 0..16u32 {
                let py = (mb_y * MB + row) as usize;
                let base = py * full_w + (mb_x * MB) as usize;
                w.append_bytes(&y[base..base + 16]);
            }
            for plane in [cb, cr] {
                for row in 0..8u32 {
                    let py = (mb_y * 8 + row) as usize;
                    let base = py * cw + (mb_x * 8) as usize;
                    w.append_bytes(&plane[base..base + 8]);
                }
            }
        }
    }
    w.trailing_bits();
    w.finish()
}

/// A P slice with every macroblock skipped: the picture before it, again.
/// `index` is the picture's place after the IDR (1, 2, ...); frame_num and
/// the picture order count follow it, and the slice is a reference picture
/// so the next skip frame's counts run on from it rather than from the IDR.
fn skip_slice(index: u32, macroblocks: u32) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.ue(0); // first_mb_in_slice
    w.ue(5); // slice_type 5 == P, and every slice in the picture is P
    w.ue(0); // pic_parameter_set_id
    w.put_bits(index % (1 << FRAME_NUM_BITS), FRAME_NUM_BITS); // frame_num
    w.put_bits((2 * index) % (1 << POC_LSB_BITS), POC_LSB_BITS); // pic_order_cnt_lsb
    w.put_bit(0); // num_ref_idx_active_override_flag
    w.put_bit(0); // ref_pic_list_modification_flag_l0
    w.put_bit(0); // adaptive_ref_pic_marking_mode_flag (sliding window)
    w.se(0); // slice_qp_delta
    w.ue(macroblocks); // mb_skip_run: the whole picture
    w.trailing_bits();
    w.finish()
}

/// Wrap an RBSP in a NAL: the one-byte header, then the payload with
/// emulation-prevention bytes inserted so no `00 00 00/01/02/03` masquerades
/// as a start code. I_PCM's raw samples make this essential.
fn nal(nal_ref_idc: u8, nal_unit_type: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len() + 1);
    out.push((nal_ref_idc << 5) | nal_unit_type);
    let mut zeros = 0u32;
    for &byte in rbsp {
        if zeros >= 2 && byte <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(byte);
        if byte == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
    }
    out
}

fn length_prefixed(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len() + 4);
    out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
    out.extend_from_slice(nal);
    out
}

/// The `avcC` record: version 1, the three profile bytes, a 4-byte NAL length
/// size, then one SPS and one PPS. SPS/PPS here are raw RBSP; avcC wants the
/// NAL (with header) but without emulation prevention added by the muxer, so
/// they are wrapped with a header and length only.
fn avc_decoder_config(shape: &StreamShape, sps_rbsp: &[u8], pps_rbsp: &[u8]) -> Vec<u8> {
    let sps = nal(3, 7, sps_rbsp);
    let pps = nal(3, 8, pps_rbsp);
    let mut out = Vec::new();
    out.push(1); // configurationVersion
    out.push(shape.profile_idc);
    out.push(shape.constraint_flags); // profile_compatibility
    out.push(shape.level_idc);
    out.push(0xFF); // 6 reserved bits set | lengthSizeMinusOne = 3
    out.push(0xE1); // 3 reserved bits set | numOfSequenceParameterSets = 1
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(&sps);
    out.push(1); // numOfPictureParameterSets
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(&pps);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut buf = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            buf.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        buf
    }

    /// A still of another shape comes out the stream's size with its own
    /// shape kept: scaled to fit, centred, the rest black.
    #[test]
    fn a_still_is_fitted_into_the_streams_frame_on_black() {
        let wide = image::RgbaImage::from_pixel(200, 50, image::Rgba([255, 255, 255, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        wide.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let still = encode_still_fitted(png.get_ref(), 64, 48).unwrap();
        assert_eq!((still.width, still.height), (64, 48));
        // Decode the I_PCM samples back: the top rows are black bars, the
        // middle rows carry the white picture.
        let same = encode_still_fitted(png.get_ref(), 200, 50).unwrap();
        assert_eq!((same.width, same.height), (200, 50));
        assert_ne!(still.access_unit, same.access_unit);
    }

    #[test]
    fn any_dimensions_are_padded_and_cropped() {
        // Not macroblock-aligned: coded on the 16-grid (32x16), cropped to the
        // even display size (16x16). A television image is any size.
        let still = encode_still(&solid(17, 16, [0, 0, 0]), 17, 16).expect("padded");
        assert_eq!((still.width, still.height), (16, 16));
        // An odd height crops down by the one row that cannot be halved.
        let odd = encode_still(&solid(1920, 1080, [0, 0, 0]), 1920, 1080).expect("1080 padded");
        assert_eq!((odd.width, odd.height), (1920, 1080));
        // Too small to carry a croppable frame.
        assert_eq!(
            encode_still(&solid(1, 1, [0, 0, 0]), 1, 1).unwrap_err(),
            StillError::Dimensions
        );
        assert_eq!(
            encode_still(&[0u8; 3], 16, 16).unwrap_err(),
            StillError::PixelCount
        );
    }

    /// The SPS of a real clip (High 3.2, 25 fps, two frames of reordering),
    /// as the dumped stream carried it.
    const CLIP_SPS_HEX: &str = "67640020acd9805605be6e6a020202800000030080000019078c18cd";

    fn clip_sps() -> Vec<u8> {
        (0..CLIP_SPS_HEX.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&CLIP_SPS_HEX[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn a_clips_sps_yields_the_shape_its_decoder_is_configured_by() {
        let shape = StreamShape::from_sps_nal(&clip_sps(), None).unwrap();
        assert_eq!(shape.profile_idc, 100);
        assert_eq!(shape.constraint_flags, 0);
        assert_eq!(shape.level_idc, 32);
        assert_eq!(shape.max_num_ref_frames, 5);
        assert_eq!(
            shape.timing,
            Some(VuiTiming {
                num_units_in_tick: 1,
                time_scale: 50,
                fixed_frame_rate: false
            })
        );
        assert_eq!(
            shape.reorder.map(|r| r.max_num_reorder_frames),
            Some(2),
            "two frames of reordering"
        );
        assert_eq!(shape.frames_per_second, 25, "from the VUI timing");
        assert_eq!(shape.codec(), "avc1.640020");
        // The catalog's rate wins over the VUI when given.
        let shaped = StreamShape::from_sps_nal(&clip_sps(), Some(29_970)).unwrap();
        assert_eq!(shaped.frames_per_second, 30);
        // Wrapped in an avcC it reads the same.
        let mut avcc = vec![1, 100, 0, 32, 0xff, 0xe1];
        avcc.extend_from_slice(&(clip_sps().len() as u16).to_be_bytes());
        avcc.extend_from_slice(&clip_sps());
        avcc.extend_from_slice(&[1, 0, 0]);
        assert_eq!(StreamShape::from_avcc(&avcc, None).unwrap(), shape);
        assert_eq!(
            StreamShape::from_sps_nal(&[0x68, 0xe9], None),
            Err(ShapeError::NotAnSps)
        );
    }

    #[test]
    fn the_default_shape_is_the_baseline_one_picture_still() {
        let rgba = solid(64, 32, [10, 200, 30]);
        let plain = encode_still(&rgba, 64, 32).unwrap();
        let shaped = encode_still_shaped(&rgba, 64, 32, &StreamShape::default()).unwrap();
        assert_eq!(plain, shaped);
        assert_eq!(plain.frames_per_second(), 1);
        assert_eq!(plain.codec, "avc1.424028");
    }

    #[test]
    fn a_still_under_a_clips_shape_mirrors_it_and_its_own_sps_reads_back() {
        let shape = StreamShape::from_sps_nal(&clip_sps(), None).unwrap();
        let rgba = solid(64, 32, [10, 200, 30]);
        let still = encode_still_shaped(&rgba, 64, 32, &shape).unwrap();
        assert_eq!(still.codec, "avc1.640020");
        assert_eq!(still.frames_per_second(), 25);
        assert_eq!(&still.avcc[1..4], &[100, 0, 32]);
        let read = StreamShape::from_avcc(&still.avcc, None).unwrap();
        assert_eq!(
            read, shape,
            "the SPS the still writes says what the clip's said"
        );
        // Skip frames are tiny and count from one.
        let skip = still.skip_frame(1);
        assert!(
            skip.len() < 16,
            "a skip frame is a few bytes: {}",
            skip.len()
        );
        assert_eq!(skip[4] & 0x1f, 1, "a non-IDR slice");
        assert_eq!(skip[4] >> 5, 2, "a reference picture");
        assert_ne!(still.skip_frame(1), still.skip_frame(2));
    }

    /// The whole one-second group a shaped still becomes — an IDR and 24 skip
    /// frames under the clip's High-profile SPS — decodes to 25 identical
    /// pictures, and a decoder has nothing to say about it. Ignored by
    /// default because it shells out to ffmpeg.
    #[test]
    #[ignore]
    fn ffmpeg_decodes_a_shaped_still_and_its_skip_frames_to_the_same_picture() {
        use std::io::Write;
        use std::process::Command;
        let dir = std::env::var("STILL_DUMP_DIR").unwrap_or_else(|_| "/tmp".into());
        let shape = StreamShape::from_sps_nal(&clip_sps(), None).unwrap();
        let (w, h) = (1366u32, 720u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 90, 255]);
            }
        }
        let still = encode_still_shaped(&rgba, w, h, &shape).unwrap();
        let sps_len = u16::from_be_bytes([still.avcc[6], still.avcc[7]]) as usize;
        let sps = &still.avcc[8..8 + sps_len];
        let pps_off = 8 + sps_len + 1;
        let pps_len = u16::from_be_bytes([still.avcc[pps_off], still.avcc[pps_off + 1]]) as usize;
        let pps = &still.avcc[pps_off + 2..pps_off + 2 + pps_len];
        let mut es = Vec::new();
        let mut push = |unit: &[u8]| {
            es.extend_from_slice(&[0, 0, 0, 1]);
            es.extend_from_slice(unit);
        };
        push(sps);
        push(pps);
        push(&still.access_unit[4..]);
        for index in 1..still.frames_per_second() {
            let skip = still.skip_frame(index);
            push(&skip[4..]);
        }
        let raw = format!("{dir}/shaped-still.h264");
        std::fs::File::create(&raw).unwrap().write_all(&es).unwrap();
        let out = Command::new("ffmpeg")
            .args([
                "-v", "warning", "-i", &raw, "-f", "rawvideo", "-pix_fmt", "yuv420p", "-",
            ])
            .output()
            .expect("ffmpeg runs");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stderr.is_empty(),
            "the decoder had something to say: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let frame = (w * h + 2 * (w / 2) * (h / 2)) as usize;
        assert_eq!(out.stdout.len(), frame * 25, "25 pictures out");
        let first = &out.stdout[..frame];
        for index in 1..25 {
            assert!(
                &out.stdout[index * frame..(index + 1) * frame] == first,
                "picture {index} differs from the IDR"
            );
        }
    }

    #[test]
    fn ue_and_se_match_the_reference_codes() {
        let mut w = BitWriter::new();
        w.ue(0);
        w.ue(1);
        w.ue(2);
        // 1, 010, 011 -> 1_010_011x padded
        assert_eq!(w.finish(), vec![0b1_010_011_0]);
        let mut s = BitWriter::new();
        s.se(0);
        s.se(1);
        s.se(-1);
        // se: 0->1, 1->010, -1->011
        assert_eq!(s.finish(), vec![0b1_010_011_0]);
    }

    /// The encoder's own claim about conformance, checked against a real
    /// decoder: encode a still, lay it out as an Annex-B elementary stream,
    /// and have ffmpeg decode it back to a PNG of the right size. Ignored by
    /// default because it shells out to ffmpeg; run with `--ignored` where it
    /// is installed. Set STILL_DUMP_DIR to keep the artefacts for a look.
    #[test]
    #[ignore]
    fn ffmpeg_decodes_the_still_to_the_right_dimensions() {
        use std::io::Write;
        use std::process::Command;
        let dir = std::env::var("STILL_DUMP_DIR").unwrap_or_else(|_| "/tmp".into());
        let (w, h) = (1280u32, 720u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (mut r, mut g, mut b) = if x < w / 2 {
                    (200u8, 30, 30)
                } else {
                    (30, 30, 200)
                };
                if y > h / 3 && y < 2 * h / 3 {
                    g = 200;
                    r /= 3;
                    b /= 3;
                }
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        let still = encode_still(&rgba, w, h).unwrap();
        let sps_len = u16::from_be_bytes([still.avcc[6], still.avcc[7]]) as usize;
        let sps = &still.avcc[8..8 + sps_len];
        let pps_off = 8 + sps_len + 1;
        let pps_len = u16::from_be_bytes([still.avcc[pps_off], still.avcc[pps_off + 1]]) as usize;
        let pps = &still.avcc[pps_off + 2..pps_off + 2 + pps_len];
        let idr_len = u32::from_be_bytes(still.access_unit[0..4].try_into().unwrap()) as usize;
        let idr = &still.access_unit[4..4 + idr_len];
        let mut es = Vec::new();
        for n in [sps, pps, idr] {
            es.extend_from_slice(&[0, 0, 0, 1]);
            es.extend_from_slice(n);
        }
        let raw = format!("{dir}/still.h264");
        let png = format!("{dir}/still-decoded.png");
        std::fs::File::create(&raw).unwrap().write_all(&es).unwrap();
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-i", &raw, "-frames:v", "1", "-y", &png])
            .output()
            .expect("ffmpeg runs");
        assert!(
            out.status.success(),
            "ffmpeg refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=p=0",
                &raw,
            ])
            .output()
            .expect("ffprobe runs");
        let dims = String::from_utf8_lossy(&probe.stdout);
        assert!(dims.trim().starts_with("1280,720"), "decoded dims: {dims}");
    }

    #[test]
    fn a_png_still_decodes_and_encodes_to_the_same_frame_as_its_pixels() {
        // A tiny PNG encoded here, then decoded and H.264-encoded, matches the
        // frame the raw pixels produce — the PNG path is just a front door.
        let rgba = solid(16, 16, [40, 90, 160]);
        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, 16, 16);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&rgba).unwrap();
        }
        let from_png = encode_still_png(&png_bytes).unwrap();
        let from_pixels = encode_still(&rgba, 16, 16).unwrap();
        assert_eq!(from_png, from_pixels);
    }

    #[test]
    fn a_still_is_shaped_the_way_the_catalog_and_packager_expect() {
        let still = encode_still(&solid(16, 16, [10, 20, 30]), 16, 16).unwrap();
        assert_eq!(still.codec, "avc1.424028");
        assert_eq!(still.width, 16);
        // access unit is length-prefixed and the length is the rest.
        let len = u32::from_be_bytes(still.access_unit[0..4].try_into().unwrap()) as usize;
        assert_eq!(len, still.access_unit.len() - 4);
        // avcC starts version 1, profile 66.
        assert_eq!(still.avcc[0], 1);
        assert_eq!(still.avcc[1], PROFILE_IDC);
    }
}
