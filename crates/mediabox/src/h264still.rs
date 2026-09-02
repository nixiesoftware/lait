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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StillError {
    /// A dimension is zero, not a multiple of 16, or beyond the level's bound.
    Dimensions,
    /// The pixel buffer is not `width * height * 4` bytes of RGBA.
    PixelCount,
    /// The PNG bytes could not be decoded.
    Png,
}

/// Encode a rendered still, delivered as a PNG (what the World produces and
/// the coordinator holds as a frame asset), into one H.264 IDR frame. The PNG
/// is decoded to RGBA and handed to [`encode_still`]; its dimensions must be
/// multiples of 16, which a panel-sized render already is.
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

const PROFILE_IDC: u8 = 66; // Constrained Baseline
const LEVEL_IDC: u8 = 40; // 4.0 — comfortably covers 1080p stills
const MB: u32 = 16;

/// Encode an RGBA still (`width * height * 4` bytes, row-major, no padding)
/// into one H.264 IDR frame. Dimensions must be multiples of 16.
pub fn encode_still(rgba: &[u8], width: u32, height: u32) -> Result<StillH264, StillError> {
    if width == 0 || height == 0 || width % MB != 0 || height % MB != 0 {
        return Err(StillError::Dimensions);
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(StillError::Dimensions)?;
    if rgba.len() != expected {
        return Err(StillError::PixelCount);
    }

    let (y_plane, cb_plane, cr_plane) = rgba_to_yuv420(rgba, width, height);

    let sps = sequence_parameter_set(width, height);
    let pps = picture_parameter_set();
    let slice = idr_slice(&y_plane, &cb_plane, &cr_plane, width, height);

    let codec = format!("avc1.{PROFILE_IDC:02x}00{LEVEL_IDC:02x}");
    let avcc = avc_decoder_config(&sps, &pps);
    let access_unit = length_prefixed(&nal(3, 5, &slice));

    Ok(StillH264 {
        codec,
        avcc,
        access_unit,
        width,
        height,
    })
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

fn sequence_parameter_set(width: u32, height: u32) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.put_bits(u32::from(PROFILE_IDC), 8);
    // constraint_set0..5 flags + 2 reserved zero bits. Constrained Baseline
    // sets flag1 (obeys Main's common subset); the rest are zero.
    w.put_bits(0b0100_0000, 8);
    w.put_bits(u32::from(LEVEL_IDC), 8);
    w.ue(0); // seq_parameter_set_id
    w.ue(0); // log2_max_frame_num_minus4  -> frame_num is 4 bits
    w.ue(0); // pic_order_cnt_type
    w.ue(0); // log2_max_pic_order_cnt_lsb_minus4 -> poc_lsb is 4 bits
    w.ue(0); // max_num_ref_frames
    w.put_bit(0); // gaps_in_frame_num_value_allowed_flag
    w.ue(width / MB - 1); // pic_width_in_mbs_minus1
    w.ue(height / MB - 1); // pic_height_in_map_units_minus1
    w.put_bit(1); // frame_mbs_only_flag
    w.put_bit(0); // direct_8x8_inference_flag
    w.put_bit(0); // frame_cropping_flag (dimensions are 16-aligned)
    w.put_bit(0); // vui_parameters_present_flag
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
    w.put_bits(0, 4); // frame_num (log2_max_frame_num == 4)
    w.ue(0); // idr_pic_id
    w.put_bits(0, 4); // pic_order_cnt_lsb (log2 == 4)
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
fn avc_decoder_config(sps_rbsp: &[u8], pps_rbsp: &[u8]) -> Vec<u8> {
    let sps = nal(3, 7, sps_rbsp);
    let pps = nal(3, 8, pps_rbsp);
    let mut out = Vec::new();
    out.push(1); // configurationVersion
    out.push(PROFILE_IDC);
    out.push(0); // profile_compatibility (constraint flags) — 0 is safe here
    out.push(LEVEL_IDC);
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

    #[test]
    fn dimensions_must_be_macroblock_aligned() {
        assert_eq!(
            encode_still(&solid(17, 16, [0, 0, 0]), 17, 16).unwrap_err(),
            StillError::Dimensions
        );
        assert_eq!(
            encode_still(&[0u8; 3], 16, 16).unwrap_err(),
            StillError::PixelCount
        );
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
        assert_eq!(still.codec, "avc1.420028");
        assert_eq!(still.width, 16);
        // access unit is length-prefixed and the length is the rest.
        let len = u32::from_be_bytes(still.access_unit[0..4].try_into().unwrap()) as usize;
        assert_eq!(len, still.access_unit.len() - 4);
        // avcC starts version 1, profile 66.
        assert_eq!(still.avcc[0], 1);
        assert_eq!(still.avcc[1], PROFILE_IDC);
    }
}
