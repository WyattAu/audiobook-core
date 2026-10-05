//! MPEG-1/2/2.5 Layer I–III frame walking.
//!
//! An MP3 has no duration field. Its duration is derived by walking frame
//! headers, and this module is that walk: locate the first frame, then step
//! through them by their declared lengths until the stream ends.
//!
//! # The frame header
//!
//! Four bytes, all big-endian:
//!
//! ```text
//! byte 0   0xFF                      frame sync, 11 bits
//! byte 1   111 VV LL P               version, layer, protection
//! byte 2   BBBB SS Pd Pr             bitrate, sample rate, padding
//! byte 3   CC MM .. ..               channel mode, ...
//! ```
//!
//! Two fields are traps. **Padding** adds one byte to a Layer II/III frame and
//! four to a Layer I frame, so a CBR file's byte size is *not* `frames ×
//! constant` — ignoring the bit drifts the duration by up to 1.5%. And **bitrate
//! index 0** means "free" and index 15 means "bad", so neither is a rate.
//!
//! # Xing and VBRI
//!
//! A VBR file's encoder writes the true frame count into the first frame, after
//! the side information. That count is exact, so it is preferred over any
//! derivation. The offset depends on version and channel mode, which is why
//! [`vbr_tag_offset`] is a table rather than a constant.
//!
//! The two headers disagree on purpose: **`Info`** means a CBR file that
//! bothered to write the header, and **`Xing`** means genuinely variable. Both
//! carry a frame count, and this module reports the distinction in
//! `variable_bitrate` — a player that shows "VBR" for an `Info` file is wrong.

use crate::container::ContainerKind;

/// The MPEG audio version an encoding uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MpegVersion {
    /// MPEG-1 — CD-era, 32–320 kbit/s.
    Mpeg1,
    /// MPEG-2 — half the sample rates, 8–160 kbit/s.
    Mpeg2,
    /// MPEG-2.5 — a quarter of MPEG-1's rates, for very low bitrates.
    Mpeg25,
    /// A reserved encoding value that must be rejected.
    Reserved,
}

impl MpegVersion {
    /// The base sample rates, in Hz, before the two-bit index is applied.
    #[must_use]
    const fn base_sample_rates(self) -> [u32; 3] {
        match self {
            MpegVersion::Mpeg1 => [44_100, 48_000, 32_000],
            MpegVersion::Mpeg2 => [22_050, 24_000, 16_000],
            MpegVersion::Mpeg25 => [11_025, 12_000, 8_000],
            MpegVersion::Reserved => [0, 0, 0],
        }
    }

    /// Whether this is MPEG-1, which uses wider side information and more
    /// samples per frame.
    #[must_use]
    const fn is_v1(self) -> bool {
        matches!(self, MpegVersion::Mpeg1)
    }
}

/// The audio layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Layer {
    /// Layer I — rarely seen.
    Layer1,
    /// Layer II.
    Layer2,
    /// Layer III — MP3.
    Layer3,
    /// A reserved value that must be rejected.
    Reserved,
}

impl Layer {
    /// Samples per frame. Layer III halves its count on MPEG-2 and below.
    #[must_use]
    const fn samples_per_frame(self, version: MpegVersion) -> u32 {
        match self {
            Layer::Layer1 => 384,
            Layer::Layer2 => 1_152,
            Layer::Layer3 => {
                if version.is_v1() {
                    1_152
                } else {
                    576
                }
            }
            Layer::Reserved => 0,
        }
    }
}

/// A parsed frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// The MPEG version.
    pub version: MpegVersion,
    /// The audio layer.
    pub layer: Layer,
    /// Average bitrate in bits per second.
    pub bitrate_bps: u32,
    /// Samples per second.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
    /// Whether this frame is longer by its padding slot.
    pub padding: bool,
}

impl FrameHeader {
    /// This frame's length in bytes, header included.
    ///
    /// The padding bit is why this cannot be derived from the bitrate alone. It
    /// adds **one byte** for Layer II and III and **four** for Layer I, because
    /// Layer I's formula multiplies by four *after* adding the bit. Ignoring it
    /// is exactly the drift that makes a naively-derived duration wrong.
    #[must_use]
    pub fn frame_len(&self) -> usize {
        let bitrate = self.bitrate_bps as usize;
        let rate = self.sample_rate as usize;
        if bitrate == 0 || rate == 0 {
            return 0;
        }
        let pad = usize::from(self.padding);
        let len = match (self.layer, self.version.is_v1()) {
            (Layer::Layer1, _) => (12_000 * bitrate / rate + pad) * 4,
            (Layer::Layer2, _) => 144 * bitrate / rate + pad,
            (Layer::Layer3, true) => 144 * bitrate / rate + pad,
            (Layer::Layer3, false) => 72 * bitrate / rate + pad,
            (Layer::Reserved, _) => 0,
        };
        // A frame is never shorter than its own header.
        if len < 4 {
            0
        } else {
            len
        }
    }
}

/// Bitrates for Layer I, in kbit/s, indexed by the 4-bit field.
const L1_V1: [u32; 15] = [
    0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
];
const L2_V1: [u32; 15] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
];
const L3_V1: [u32; 15] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const L1_LOW: [u32; 15] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
];
const L23_LOW: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

/// Look up a bitrate from the tables.
///
/// Index 0 is "free" and 15 is "bad" — neither is a rate, so both yield `None`
/// rather than a plausible number.
#[must_use]
fn bitrate_table(version: MpegVersion, layer: Layer, index: u8) -> Option<u32> {
    let table: &[u32; 15] = match (layer, version.is_v1()) {
        (Layer::Layer1, true) => &L1_V1,
        (Layer::Layer1, false) => &L1_LOW,
        (Layer::Layer2, true) => &L2_V1,
        (Layer::Layer2, false) => &L23_LOW,
        (Layer::Layer3, true) => &L3_V1,
        (Layer::Layer3, false) => &L23_LOW,
        (Layer::Reserved, _) => return None,
    };
    if index == 0 || index > 14 {
        return None;
    }
    table.get(usize::from(index)).map(|kbps| kbps * 1_000)
}

/// Parse a frame header at the start of `data`.
///
/// Returns `None` for anything that is not a valid header, which is what keeps
/// a walk from following a false sync into the middle of a frame.
#[must_use]
pub fn parse_header(data: &[u8]) -> Option<FrameHeader> {
    let b0 = *data.first()?;
    let b1 = *data.get(1)?;
    let b2 = *data.get(2)?;
    let b3 = *data.get(3)?;

    if b0 != 0xFF || b1 & 0xE0 != 0xE0 {
        return None;
    }

    let version = match (b1 >> 3) & 0b11 {
        0b00 => MpegVersion::Mpeg25,
        0b01 => MpegVersion::Reserved,
        0b10 => MpegVersion::Mpeg2,
        _ => MpegVersion::Mpeg1,
    };
    let layer = match (b1 >> 1) & 0b11 {
        0b01 => Layer::Layer3,
        0b10 => Layer::Layer2,
        0b11 => Layer::Layer1,
        _ => Layer::Reserved,
    };
    if version == MpegVersion::Reserved || layer == Layer::Reserved {
        return None;
    }

    let br_index = b2 >> 4;
    let sr_index = (b2 >> 2) & 0b11;
    if sr_index == 0b11 {
        // A reserved sample-rate index is not a frame.
        return None;
    }
    let bitrate_bps = bitrate_table(version, layer, br_index)?;
    let sample_rate = *version.base_sample_rates().get(usize::from(sr_index))?;
    if sample_rate == 0 {
        return None;
    }

    // Channel mode: 3 (0b11) is mono, everything else is two channels. Modes 0
    // and 1 are joint and dual stereo but still two channels.
    let mode = b3 >> 6;
    let channels = if mode == 0b11 { 1 } else { 2 };

    Some(FrameHeader {
        version,
        layer,
        bitrate_bps,
        sample_rate,
        channels,
        padding: b2 & 0b10 != 0,
    })
}

/// The offset of a Xing or Info tag from the start of its frame.
///
/// Side-information width is what makes this a table: MPEG-1 stereo has 32
/// bytes of it, MPEG-2/2.5 mono has 8, and everything in between differs.
#[must_use]
pub fn vbr_tag_offset(version: MpegVersion, channels: u16) -> usize {
    // Layer I has no side information at all; the header is immediately
    // followed by the (unused here) payload, so 4 is the correct answer for it.
    if version.is_v1() {
        if channels == 1 {
            4 + 17
        } else {
            4 + 32
        }
    } else if channels == 1 {
        4 + 8
    } else {
        4 + 16
    }
}

/// A declared frame and byte count from a Xing, Info, or VBRI header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VbrHeader {
    /// Total frames, when the header declared one.
    pub frames: Option<u64>,
    /// Total audio bytes, when the header declared one.
    pub bytes: Option<u64>,
    /// `true` for a Xing tag, `false` for Info, `None` for VBRI.
    ///
    /// Info means the encoder wrote the header on a CBR stream, so treating it as
    /// VBR mislabels the file.
    pub is_xing: Option<bool>,
}

/// Parse a Xing, Info, or VBRI header sitting at `offset` in `data`.
#[must_use]
pub fn parse_vbr(data: &[u8], offset: usize) -> Option<VbrHeader> {
    let tag = data.get(offset..offset + 4)?;
    match tag {
        b"Xing" => Some(parse_xing_body(data.get(offset + 4..)?, true)),
        b"Info" => Some(parse_xing_body(data.get(offset + 4..)?, false)),
        b"VBRI" => Some(parse_vbri_body(data.get(offset + 4..)?)),
        _ => None,
    }
}

/// Parse the body after a `Xing`/`Info` magic.
fn parse_xing_body(body: &[u8], is_xing: bool) -> VbrHeader {
    // Flags(4) then the fields, each present only when its flag bit is set:
    // bit 0 frames, bit 1 bytes, bit 2 TOC, bit 3 quality.
    let flags = body.get(..4).and_then(|f| <[u8; 4]>::try_from(f).ok());
    let Some(flags) = flags.map(u32::from_be_bytes) else {
        return VbrHeader {
            is_xing: Some(is_xing),
            ..VbrHeader::default()
        };
    };
    let mut cursor = 4usize;
    let mut frames = None;
    let mut bytes = None;

    if flags & 0b0001 != 0 {
        frames = body
            .get(cursor..cursor + 4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(|b| u64::from(u32::from_be_bytes(b)));
        cursor += 4;
    }
    if flags & 0b0010 != 0 {
        bytes = body
            .get(cursor..cursor + 4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(|b| u64::from(u32::from_be_bytes(b)));
    }

    VbrHeader {
        frames,
        bytes,
        is_xing: Some(is_xing),
    }
}

/// Parse the body after a `VBRI` magic.
///
/// The layout is fixed and version 1 is the only one that exists: version(2),
/// delay(2), quality(2), bytes(4), frames(4), toc entries(2), scale(2), entry
/// size(2), frames per entry(2).
fn parse_vbri_body(body: &[u8]) -> VbrHeader {
    let bytes = body
        .get(6..10)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map(|b| u64::from(u32::from_be_bytes(b)));
    let frames = body
        .get(10..14)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map(|b| u64::from(u32::from_be_bytes(b)));
    VbrHeader {
        frames,
        bytes,
        is_xing: None,
    }
}

/// What a walk over an MPEG audio stream found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioScan {
    /// The first frame's header, which describes the stream.
    pub header: Option<FrameHeader>,
    /// The exact frame count, when the file declared one.
    pub frame_count: Option<u64>,
    /// Frames actually walked, which is *not* the duration for a truncated file.
    pub frames_walked: u64,
    /// Bytes of audio, excluding any ID3 tag.
    pub audio_bytes: u64,
    /// Whether the file is genuinely variable bitrate.
    pub variable_bitrate: bool,
    /// The stream's declared sample rate.
    pub sample_rate: u32,
    /// The stream's channel count.
    pub channels: u16,
    /// The bitrate every frame declared, when they all agreed.
    pub bitrate_bps: u64,
}

impl AudioScan {
    /// Duration in milliseconds from a frame count.
    ///
    /// Returns `0` when there is nothing to compute from, rather than guessing.
    #[must_use]
    pub fn samples_from_frames(&self, frames: u64) -> u64 {
        let Some(h) = self.header else {
            return 0;
        };
        let per_frame = h.layer.samples_per_frame(h.version) as u64;
        if h.sample_rate == 0 {
            return 0;
        }
        frames.saturating_mul(per_frame).saturating_mul(1_000) / u64::from(h.sample_rate)
    }

    /// Duration in milliseconds.
    ///
    /// Prefers the declared frame count over the frames walked. A file that
    /// declares its count is not walked frame by frame at all — the walk would
    /// read every frame to learn a number the header already stated — so
    /// `frames_walked` is 0 there and using it would report a zero-length file.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.samples_from_frames(self.frame_count.unwrap_or(self.frames_walked))
    }
}

/// Locate the first frame in `data`, skipping an ID3 tag if present.
///
/// Returns the offset and the parsed header.
#[must_use]
pub fn first_frame(data: &[u8], kind: ContainerKind) -> Option<(usize, FrameHeader)> {
    let start = match kind {
        ContainerKind::Mp3Id3 => id3_core::find_id3v2(data)?,
        _ => 0,
    };
    // Skip any padding or APE tag that sits between the ID3 tag and the audio.
    let mut cursor = start;
    let limit = data.len().saturating_sub(4);
    while cursor < limit {
        if let Some(h) = parse_header(data.get(cursor..)?) {
            return Some((cursor, h));
        }
        cursor += 1;
    }
    None
}

/// Walk an MPEG audio stream.
///
/// The walk looks for a declared frame count first — the only exact source — and
/// falls back to stepping frame by frame. Stepping is bounded, because a
/// corrupt stream can otherwise walk for as long as the buffer is large.
#[must_use]
pub fn walk(data: &[u8], kind: ContainerKind) -> Option<AudioScan> {
    /// Stop a full walk past this many frames.
    ///
    /// At the shortest legal frame (a 8 kbit/s MPEG-2.5 frame is ~24 bytes) a
    /// 200 MB file holds ~8 M frames, so a bound a little above that cannot
    /// truncate a real book while still terminating on corrupt input.
    const MAX_WALK_FRAMES: u64 = 16_000_000;

    let (start, header) = first_frame(data, kind)?;
    let audio_start = start;
    let audio_len = data.len().saturating_sub(start);

    // A declared count, if the encoder wrote one.
    let vbr_off = vbr_tag_offset(header.version, header.channels);
    let declared = parse_vbr(data, audio_start.saturating_add(vbr_off));

    let mut scan = AudioScan {
        header: Some(header),
        frames_walked: 0,
        audio_bytes: u64::try_from(audio_len).unwrap_or(u64::MAX),
        sample_rate: header.sample_rate,
        channels: header.channels,
        bitrate_bps: u64::from(header.bitrate_bps),
        ..AudioScan::default()
    };

    if let Some(v) = declared {
        if let Some(frames) = v.frames.filter(|f| *f > 0) {
            scan.frame_count = Some(frames);
            // A Xing tag means variable; an Info tag means the encoder wrote a
            // count on a constant-rate stream. VBRI does not distinguish.
            scan.variable_bitrate = v.is_xing.unwrap_or(true);
            if let Some(bytes) = v.bytes.filter(|b| *b > 0) {
                scan.audio_bytes = bytes;
            }
            return Some(scan);
        }
    }

    // No declared count: walk. Frame lengths vary with the padding bit, so each
    // step re-reads the header rather than multiplying one constant.
    let mut cursor = audio_start;
    let mut uniform_bitrate = true;
    let mut frames = 0u64;
    while frames < MAX_WALK_FRAMES {
        let Some(h) = parse_header(data.get(cursor..)?) else {
            break;
        };
        if h.sample_rate != header.sample_rate || h.channels != header.channels {
            // A real concatenation or a corrupt region; either way the stream is
            // not one homogeneous encoding from here on.
            uniform_bitrate = false;
            break;
        }
        if h.bitrate_bps != header.bitrate_bps {
            uniform_bitrate = false;
        }
        let len = h.frame_len();
        if len == 0 || cursor.saturating_add(len) > data.len() {
            break;
        }
        cursor += len;
        frames += 1;
    }
    scan.frames_walked = frames;
    scan.variable_bitrate = !uniform_bitrate;
    Some(scan)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )]
    use super::*;
    use alloc::vec::Vec;

    /// Build a 128 kbit/s 44.1 kHz MPEG-1 Layer III frame header.
    fn mpeg1_l3_header(padding: bool) -> [u8; 4] {
        header_with(9, 0, padding)
    }

    /// Build a frame header from its two index fields.
    ///
    /// Both indices are explicit because they are the two fields a hand-built
    /// header gets wrong: the bitrate index means a *different rate* on MPEG-1
    /// than on MPEG-2, and the sample-rate index is two bits at offset 2.
    fn header_with(bitrate_index: u8, sample_rate_index: u8, padding: bool) -> [u8; 4] {
        let mut b2 = (bitrate_index & 0x0F) << 4;
        b2 |= (sample_rate_index & 0b11) << 2;
        if padding {
            b2 |= 0b10;
        }
        // channel mode 1 (joint stereo), so 2 channels and 32 bytes of side info.
        [0xFF, 0xFB, b2, 0b0100_0000]
    }

    /// A frame's worth of bytes with the given header and total length.
    fn frame(header: [u8; 4], total_len: usize) -> Vec<u8> {
        let mut f = header.to_vec();
        f.resize(total_len, 0);
        f
    }

    #[test]
    fn a_canonical_header_parses() {
        let h = parse_header(&mpeg1_l3_header(false)).expect("a valid header");
        assert_eq!(h.version, MpegVersion::Mpeg1);
        assert_eq!(h.layer, Layer::Layer3);
        assert_eq!(h.bitrate_bps, 128_000);
        assert_eq!(h.sample_rate, 44_100);
        assert_eq!(h.channels, 2);
        assert!(!h.padding);
    }

    #[test]
    fn mono_is_one_channel_and_joint_stereo_is_two() {
        // Channel mode 3 is mono.
        let mut mono = mpeg1_l3_header(false);
        mono[3] = 0b1100_0000;
        assert_eq!(parse_header(&mono).expect("valid").channels, 1);
        // Mode 0 is joint stereo, mode 1 is dual — both two channels.
        for mode in [0b0000_0000u8, 0b0100_0000] {
            let mut b = mpeg1_l3_header(false);
            b[3] = mode;
            assert_eq!(parse_header(&b).expect("valid").channels, 2);
        }
    }

    #[test]
    fn every_sample_rate_index_resolves() {
        // MPEG-1: 44100, 48000, 32000.
        let expected = [44_100u32, 48_000, 32_000];
        for (i, want) in expected.iter().enumerate() {
            let h = header_with(9, i as u8, false);
            assert_eq!(parse_header(&h).expect("valid").sample_rate, *want);
        }
    }

    #[test]
    fn padding_lengthens_a_frame_by_one_byte_for_layer_iii() {
        // The drift that a bitrate-only duration calculation misses. Note it is
        // one byte for Layer III, not four — four is Layer I only.
        let bare = parse_header(&mpeg1_l3_header(false)).expect("valid");
        let padded = parse_header(&mpeg1_l3_header(true)).expect("valid");
        assert_eq!(bare.frame_len(), 417, "144 * 128000 / 44100");
        assert_eq!(padded.frame_len(), 418);
    }

    #[test]
    fn padding_adds_four_bytes_to_a_layer_i_frame() {
        // Layer I's formula multiplies by four after adding the padding bit, so
        // there the padding really is a four-byte slot.
        let mut h = mpeg1_l3_header(false);
        h[1] = 0xFB & !(0b11 << 1) | (0b11 << 1); // layer bits 11 = Layer I
        let bare = parse_header(&h).expect("valid");
        assert_eq!(bare.layer, Layer::Layer1);

        let mut padded_bytes = h;
        padded_bytes[2] |= 0b10;
        let padded = parse_header(&padded_bytes).expect("valid");
        assert_eq!(padded.frame_len() - bare.frame_len(), 4);
    }

    #[test]
    fn a_bad_sync_is_rejected() {
        assert!(parse_header(&[0x00, 0xFB, 0x90, 0x00]).is_none());
        // Second byte's top three bits are not all set.
        assert!(parse_header(&[0xFF, 0x70, 0x90, 0x00]).is_none());
        assert!(parse_header(&[0xFF, 0xFB]).is_none());
        assert!(parse_header(&[]).is_none());
    }

    #[test]
    fn reserved_versions_and_layers_are_rejected() {
        // Version bits 01 are reserved.
        let mut h = mpeg1_l3_header(false);
        h[1] = 0xE0 | (0b01 << 3) | (0b01 << 1);
        assert!(parse_header(&h).is_none());

        // Layer bits 00 are reserved.
        let mut h = mpeg1_l3_header(false);
        h[1] = 0xFB & !(0b11 << 1);
        assert!(parse_header(&h).is_none());

        // Sample-rate index 11 is reserved.
        let mut h = mpeg1_l3_header(false);
        h[2] |= 0b1100;
        assert!(parse_header(&h).is_none());
    }

    #[test]
    fn free_and_bad_bitrate_indices_are_not_rates() {
        // Index 0 means "free format" and 15 means "bad" — neither is a rate, and
        // neither may become a plausible number.
        let mut h = mpeg1_l3_header(false);
        h[2] &= 0x0F; // bitrate index 0
        assert!(parse_header(&h).is_none());

        let h = header_with(15, 0, false);
        assert!(parse_header(&h).is_none());
    }

    #[test]
    fn mpeg2_halves_the_sample_rate_and_frame_samples() {
        // Version bits 10 = MPEG-2. The bitrate table is a *different* one below
        // MPEG-1, so index 12 is 128 kbit/s here where index 9 was.
        let mut h = header_with(12, 0, false);
        h[1] = 0xE0 | (0b10 << 3) | (0b01 << 1) | 1;
        let parsed = parse_header(&h).expect("valid");
        assert_eq!(parsed.version, MpegVersion::Mpeg2);
        assert_eq!(parsed.sample_rate, 22_050);
        assert_eq!(parsed.bitrate_bps, 128_000);
        // Layer III halves samples per frame below MPEG-1.
        assert_eq!(parsed.layer.samples_per_frame(parsed.version), 576);
        // 72 * 128000 / 22050 = 417
        assert_eq!(parsed.frame_len(), 417);
    }

    #[test]
    fn the_low_bitrate_table_differs_from_the_mpeg1_table() {
        // The same 4-bit index means different rates on MPEG-1 and MPEG-2, which
        // is why the table is selected by version and layer rather than shared.
        let v1 = bitrate_table(MpegVersion::Mpeg1, Layer::Layer3, 9);
        let v2 = bitrate_table(MpegVersion::Mpeg2, Layer::Layer3, 9);
        assert_eq!(v1, Some(128_000));
        assert_eq!(v2, Some(80_000));
    }

    #[test]
    fn the_vbr_offset_table_matches_the_side_information_widths() {
        // MPEG-1: 17 bytes mono, 32 stereo.
        assert_eq!(vbr_tag_offset(MpegVersion::Mpeg1, 1), 21);
        assert_eq!(vbr_tag_offset(MpegVersion::Mpeg1, 2), 36);
        // MPEG-2: 8 mono, 16 stereo.
        assert_eq!(vbr_tag_offset(MpegVersion::Mpeg2, 1), 12);
        assert_eq!(vbr_tag_offset(MpegVersion::Mpeg2, 2), 20);
        // MPEG-2.5 shares MPEG-2's widths.
        assert_eq!(vbr_tag_offset(MpegVersion::Mpeg25, 1), 12);
    }

    #[test]
    fn a_xing_tag_declares_frames_and_bytes() {
        // Flags: frames + bytes.
        let mut body = b"Xing".to_vec();
        body.extend_from_slice(&[0u8, 0, 0, 0b0000_0011]); // big-endian flags
        body.extend_from_slice(&1_000u32.to_be_bytes());
        body.extend_from_slice(&418_000u32.to_be_bytes());

        let v = parse_vbr(&body, 0).expect("a Xing header");
        assert_eq!(v.frames, Some(1_000));
        assert_eq!(v.bytes, Some(418_000));
        assert_eq!(v.is_xing, Some(true));
    }

    #[test]
    fn an_info_tag_is_not_reported_as_vbr() {
        // A CBR stream with a count header: Info, not Xing.
        let mut body = b"Info".to_vec();
        body.extend_from_slice(&[0u8, 0, 0, 1]); // big-endian: frames flag only
        body.extend_from_slice(&500u32.to_be_bytes());
        let v = parse_vbr(&body, 0).expect("an Info header");
        assert_eq!(v.frames, Some(500));
        assert_eq!(v.bytes, None);
        assert_eq!(v.is_xing, Some(false));
    }

    #[test]
    fn a_vbri_tag_declares_frames_and_bytes() {
        let mut body = b"VBRI".to_vec();
        body.extend_from_slice(&[0u8; 6]); // version, delay, quality
        body.extend_from_slice(&400_000u32.to_be_bytes()); // bytes
        body.extend_from_slice(&957u32.to_be_bytes()); // frames
        let v = parse_vbr(&body, 0).expect("a VBRI header");
        assert_eq!(v.frames, Some(957));
        assert_eq!(v.bytes, Some(400_000));
        assert_eq!(v.is_xing, None);
    }

    #[test]
    fn a_tag_with_no_flags_declares_nothing() {
        // All-zero flags: the header is present but carries no counts, which is
        // not the same as a count of zero.
        let mut body = b"Xing".to_vec();
        body.extend_from_slice(&[0u8; 8]);
        let v = parse_vbr(&body, 0).expect("a Xing header");
        assert_eq!(v.frames, None);
        assert_eq!(v.bytes, None);
        assert_eq!(v.is_xing, Some(true));
    }

    #[test]
    fn a_non_tag_at_the_offset_is_not_a_header() {
        assert!(parse_vbr(b"nope", 0).is_none());
        assert!(parse_vbr(b"", 0).is_none());
        // A truncated Xing body is still recognised as a Xing header, but must
        // declare nothing — not be mistaken for a count of zero.
        let v = parse_vbr(b"Xing\x00\x00", 0).expect("recognised");
        assert_eq!(v.is_xing, Some(true));
        assert_eq!(v.frames, None);
        assert_eq!(v.bytes, None);
    }

    /// A first frame carrying a Xing header at its correct offset.
    ///
    /// The header lives *inside* the frame, after the side information — placing
    /// it after the frame body is the mistake a naive implementation makes and the
    /// one that turns a VBR file into an unmeasurable one.
    fn xing_frame(magic: &[u8; 4], frames: u32, bytes: u32, with_bytes: bool) -> Vec<u8> {
        let header = mpeg1_l3_header(false);
        let channels = if header[3] >> 6 == 0b11 { 1u16 } else { 2 };
        let offset = vbr_tag_offset(MpegVersion::Mpeg1, channels);

        let mut body = magic.to_vec();
        // The flags word is big-endian, so bit 0 is the *last* byte.
        let flags = if with_bytes { 0b0000_0011u8 } else { 1u8 };
        body.extend_from_slice(&[0u8, 0, 0, flags]);
        body.extend_from_slice(&frames.to_be_bytes());
        if with_bytes {
            body.extend_from_slice(&bytes.to_be_bytes());
        }

        let mut f = header.to_vec();
        f.resize(offset, 0);
        f.extend_from_slice(&body);
        f.resize(417, 0);
        f
    }

    #[test]
    fn a_declared_frame_count_yields_an_exact_duration() {
        // 957 frames of 1152 samples at 44.1 kHz = 24.98 s.
        let mut data = xing_frame(b"Xing", 957, 400_000, true);
        data.extend_from_slice(&[0u8; 64]);

        let scan = walk(&data, ContainerKind::Mp3Bare).expect("scans");
        assert_eq!(scan.frame_count, Some(957));
        assert!(scan.variable_bitrate, "Xing means variable");
        assert_eq!(
            scan.audio_bytes, 400_000,
            "the header also declares the size"
        );
        // 957 * 1152 = 1_102_464 samples at 44_100 Hz = 24.999 s
        assert_eq!(scan.duration_ms(), 24_999);
        // With both counts known the bitrate is the file's real average, which
        // is near the nominal 128 kbit/s rather than exactly it — the whole point
        // of a VBR file having an average.
        let implied_bps = scan.audio_bytes * 8 * 1_000 / scan.duration_ms();
        let nominal = u64::from(scan.header.expect("a header").bitrate_bps);
        assert_eq!(implied_bps, 128_005);
        assert!(
            implied_bps > nominal,
            "the file averages above its nominal bitrate, which is what VBR means"
        );
    }

    #[test]
    fn a_xing_header_outside_the_frame_is_not_found() {
        // Placed after the frame body instead of inside it: a real reader finds
        // nothing, and the walk falls back to counting frames.
        let mut data = frame(mpeg1_l3_header(false), 417);
        data.extend_from_slice(b"Xing");
        data.extend_from_slice(&[0u8, 0, 0, 1]);
        data.extend_from_slice(&957u32.to_be_bytes());

        let scan = walk(&data, ContainerKind::Mp3Bare).expect("scans");
        assert_eq!(scan.frame_count, None);
        assert_eq!(
            scan.frames_walked, 1,
            "the one complete frame is still walked"
        );
    }

    #[test]
    fn a_cbr_stream_without_a_header_is_walked_frame_by_frame() {
        let mut data: Vec<u8> = Vec::new();
        for _ in 0..100 {
            data.extend_from_slice(&frame(mpeg1_l3_header(false), 417));
        }

        let scan = walk(&data, ContainerKind::Mp3Bare).expect("scans");
        assert_eq!(scan.frame_count, None, "a CBR stream declares no count");
        assert_eq!(scan.frames_walked, 100);
        assert!(!scan.variable_bitrate, "one bitrate throughout means CBR");
        assert_eq!(scan.bitrate_bps, 128_000);
        // 100 * 417 = 41_700 bytes over 128 kbit/s.
        assert_eq!(scan.duration_ms(), 2_612, "100 * 1152 samples / 44100 Hz");
    }

    #[test]
    fn padding_drift_is_visible_in_a_walk() {
        // The same 100 frames with and without padding hold different byte
        // counts, so a byte-derived duration differs — while a frame-count-derived
        // duration does not. This is the whole reason padding is read at all.
        let mut unpadded: Vec<u8> = Vec::new();
        let mut padded: Vec<u8> = Vec::new();
        for _ in 0..100 {
            unpadded.extend_from_slice(&frame(mpeg1_l3_header(false), 417));
            padded.extend_from_slice(&frame(mpeg1_l3_header(true), 418));
        }

        let a = walk(&unpadded, ContainerKind::Mp3Bare).expect("scans");
        let b = walk(&padded, ContainerKind::Mp3Bare).expect("scans");
        assert_eq!(a.frames_walked, 100);
        assert_eq!(b.frames_walked, 100);

        // Both walk the same 100 frames, so the frame-derived duration agrees.
        assert_eq!(a.duration_ms(), b.duration_ms());
        assert_eq!(
            a.duration_ms(),
            2_612,
            "100 frames * 1152 samples / 44100 Hz"
        );

        // The byte-derived durations differ by the 100 padding bytes.
        assert_eq!(b.audio_bytes - a.audio_bytes, 100);
        let da = a.audio_bytes * 8 * 1_000 / a.bitrate_bps;
        let db = b.audio_bytes * 8 * 1_000 / b.bitrate_bps;
        assert_eq!(db - da, 6, "100 bytes at 128 kbit/s is 6 ms");
    }

    #[test]
    fn a_stream_with_mixed_bitrates_is_reported_variable() {
        let mut data = frame(mpeg1_l3_header(false), 417);
        // 64 kbit/s is bitrate index 5 on MPEG-1 Layer III.
        data.extend_from_slice(&frame(header_with(5, 0, false), 208));

        let scan = walk(&data, ContainerKind::Mp3Bare).expect("scans");
        assert!(scan.variable_bitrate, "two bitrates in one stream");
    }

    #[test]
    fn a_truncated_final_frame_ends_the_walk() {
        let mut data = Vec::new();
        for _ in 0..10 {
            data.extend_from_slice(&frame(mpeg1_l3_header(false), 417));
        }
        // A valid header whose body runs past the end.
        data.extend_from_slice(&mpeg1_l3_header(false));
        data.extend_from_slice(&[0u8; 10]);

        let scan = walk(&data, ContainerKind::Mp3Bare).expect("scans");
        assert_eq!(
            scan.frames_walked, 10,
            "the short final frame is not counted"
        );
    }

    #[test]
    fn an_id3_tag_is_skipped_before_the_first_frame() {
        // Build a real tag with id3-core rather than hand-rolling the synchsafe
        // size word, so the test cannot be wrong about the format.
        let tag = id3_core::Tag::new();
        let tag_bytes = tag.to_bytes().expect("a new tag serializes");
        let audio = frame(mpeg1_l3_header(false), 417);

        let mut data = tag_bytes;
        data.extend_from_slice(&audio);

        let (offset, h) = first_frame(&data, ContainerKind::Mp3Id3).expect("finds a frame");
        assert_eq!(offset, data.len() - 417, "the walk starts after the tag");
        assert_eq!(h.bitrate_bps, 128_000);

        // Skipping the tag does not depend on the caller's declaration: the
        // byte-at-a-time sync scan lands on the same first frame either way,
        // because the tag's bytes are not a valid frame header.
        let (offset, _) = first_frame(&data, ContainerKind::Mp3Bare).expect("finds a frame");
        assert_eq!(offset, data.len() - 417, "the sync scan skips the tag too");
    }

    #[test]
    fn a_stream_with_no_valid_frame_does_not_scan() {
        assert!(walk(&[0u8; 64], ContainerKind::Mp3Bare).is_none());
        // A valid header alone is not enough: the frame body must be present.
        assert!(walk(&mpeg1_l3_header(false), ContainerKind::Mp3Bare).is_none());
    }

    #[test]
    fn samples_from_frames_is_zero_without_a_header() {
        let scan = AudioScan::default();
        assert_eq!(scan.samples_from_frames(1_000), 0);
        assert_eq!(scan.duration_ms(), 0);
    }
}
