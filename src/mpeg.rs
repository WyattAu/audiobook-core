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
/// Side-information width is what makes this a table, and the widths come from
/// ISO/IEC 11172-3 table B.1: MPEG-1 carries 17 bytes mono and 32 stereo; MPEG-2
/// and 2.5 carry 9 and 17. Adding the four-byte frame header gives 21, 36, 13 and
/// 21.
///
/// The widths are easy to get wrong by one, and getting them wrong is quiet
/// rather than loud: a tag one byte out of reach reads as *absent*, so a file
/// with a perfectly good frame count falls back to an estimate made from a
/// bitrate. The frame that estimate reads the bitrate from is the very frame the
/// encoder wrote to carry the tag, and that frame's bitrate field is a
/// placeholder. For a 22.05 kHz mono audiobook - the commonest spoken-word
/// encoding there is - the placeholder read 56 kbit/s against 32 kbit/s of real
/// audio, and the duration came out 44% short with nothing to say so.
#[must_use]
pub fn vbr_tag_offset(version: MpegVersion, channels: u16) -> usize {
    const HEADER: usize = 4;
    if version.is_v1() {
        if channels == 1 {
            HEADER + 17
        } else {
            HEADER + 32
        }
    } else if channels == 1 {
        HEADER + 9
    } else {
        HEADER + 17
    }
}

/// The offset of a VBRI tag from the start of its frame.
///
/// Always 36, whatever the version or channel count: Fraunhofer's tag sits at a
/// fixed distance, which is why a reader that looks only at the Xing offset
/// misses every VBRI file encoded for MPEG-2.
pub const VBRI_TAG_OFFSET: usize = 36;

/// The encoder's own account of samples it added at the start and end.
///
/// Every MP3 begins with a fixed encoder delay and is padded out to a whole number of
/// frames, so the samples a file *holds* are longer than the audio a player should
/// *play*. LAME records both numbers, and a reader that ignores them reports every book
/// as slightly longer than it is.
///
/// Four different version strings appear in the wild and all of them mean the same
/// thing, so all four are accepted: `LAME3.100` from LAME itself, `Lavf58` and
/// `Lavc58` from ffmpeg's muxer and encoder respectively, and `L3.99` from LAME's
/// older names. Requiring the literal `LAME` prefix — as mutagen does — is how a reader
/// silently reports 31 ms of silence at the end of every ffmpeg-encoded book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LameTag {
    /// Samples of encoder delay at the start of the file.
    pub delay_samples: u16,
    /// Samples of padding at the end of the file.
    pub padding_samples: u16,
}

impl LameTag {
    /// Whether the extended header's revision nibble is one this crate understands.
    ///
    /// A non-zero revision means the layout past this point may be different, and
    /// reading a delay out of it would be a guess wearing a number's clothes.
    #[must_use]
    fn revision_is_known(tag: &[u8]) -> bool {
        tag.get(9).map(|b| b >> 4 == 0).unwrap_or(false)
    }

    /// Whether `version` is one of the four encoder strings this crate accepts.
    #[must_use]
    fn version_is_known(version: &[u8]) -> bool {
        const PREFIXES: [&[u8]; 4] = [b"LAME", b"Lavf", b"Lavc", b"L3.99"];
        PREFIXES.iter().any(|prefix| version.starts_with(prefix))
    }

    /// Parse the LAME extended header at `offset` in `data`.
    ///
    /// `None` for anything that is not a recognised LAME header — an absent header is
    /// not an error, it just means the encoder did not say.
    #[must_use]
    pub fn parse(data: &[u8], offset: usize) -> Option<LameTag> {
        let tag = data.get(offset..offset + 24)?;
        let version = tag.get(..9)?;
        if !Self::version_is_known(version) || !Self::revision_is_known(tag) {
            return None;
        }
        // Bytes 21..24 pack the two counts: twelve bits of delay, then twelve of
        // padding, high nibble first.
        let b21 = u16::from(*tag.get(21)?);
        let b22 = u16::from(*tag.get(22)?);
        let b23 = u16::from(*tag.get(23)?);
        Some(LameTag {
            delay_samples: (b21 << 4) | (b22 >> 4),
            padding_samples: ((b22 & 0x0F) << 8) | b23,
        })
    }

    /// Total samples this tag says are not audio.
    #[must_use]
    pub fn trim_samples(self) -> u64 {
        u64::from(self.delay_samples) + u64::from(self.padding_samples)
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
    /// The encoder's declared delay and padding, when it left a LAME header.
    pub lame: Option<LameTag>,
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
        cursor += 4;
    }
    // Every optional field is skipped even when absent, because the LAME header is
    // positioned by the flags rather than by its own presence: a reader that looked for
    // `LAME` right after the byte count would find it on some files and not others.
    if flags & 0b0100 != 0 {
        cursor += 100; // seek table
    }
    if flags & 0b1000 != 0 {
        cursor += 4; // VBR quality
    }

    VbrHeader {
        frames,
        bytes,
        is_xing: Some(is_xing),
        lame: LameTag::parse(body, cursor),
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
        // A VBRI header has a delay field of its own, but Fraunhofer writes zero there
        // and mutagen does not subtract it either, so reading it would be a second
        // guess about a field no tool in the estate can corroborate.
        lame: None,
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
    /// Samples the encoder says it added before the audio.
    pub encoder_delay_samples: u32,
    /// Samples the encoder says it added after the audio.
    pub encoder_padding_samples: u32,
    /// Whether the encoder's delay and padding were applied.
    ///
    /// False when the encoder declared none, and false when what it declared is at
    /// least as long as the file it is describing — which happens on short files at
    /// low bitrates, where old LAME versions wrote delay and padding for a file that
    /// held no audio at all. Subtracting those would report a zero-length book, so
    /// they are ignored and the untrimmed count stands. `false` here means the number
    /// is not known to be the length of the *audio*; it does not mean the audio is
    /// missing.
    pub encoder_trim_applied: bool,
}

impl AudioScan {
    /// The samples a frame count holds, before any encoder trim.
    ///
    /// Kept separate from the millisecond conversion so the trim is subtracted in
    /// samples rather than in rounded milliseconds: trimming after rounding would make
    /// a one-frame trim disappear on a short file.
    #[must_use]
    pub fn samples_of(&self, frames: u64) -> u64 {
        let Some(h) = self.header else {
            return 0;
        };
        frames.saturating_mul(h.layer.samples_per_frame(h.version) as u64)
    }

    /// Duration in milliseconds from a frame count.
    ///
    /// Returns `0` when there is nothing to compute from, rather than guessing.
    #[must_use]
    pub fn samples_from_frames(&self, frames: u64) -> u64 {
        let Some(h) = self.header else {
            return 0;
        };
        if h.sample_rate == 0 {
            return 0;
        }
        self.samples_of(frames).saturating_mul(1_000) / u64::from(h.sample_rate)
    }

    /// The length of the audio, in milliseconds, with the encoder's own trim applied.
    ///
    /// This is the number a player shows and the number ffmpeg reports: the declared
    /// frame count, less the samples the encoder admits it added at each end.
    #[must_use]
    pub fn trimmed_duration_ms(&self) -> u64 {
        let frames = self.frame_count.unwrap_or(self.frames_walked);
        let raw = self.samples_of(frames);
        if !self.encoder_trim_applied {
            return self.samples_to_ms(raw);
        }
        let trim = u64::from(self.encoder_delay_samples) + u64::from(self.encoder_padding_samples);
        self.samples_to_ms(raw.saturating_sub(trim))
    }

    /// Milliseconds from a sample count at the stream's declared rate.
    #[must_use]
    fn samples_to_ms(&self, samples: u64) -> u64 {
        let Some(h) = self.header else {
            return 0;
        };
        if h.sample_rate == 0 {
            return 0;
        }
        samples.saturating_mul(1_000) / u64::from(h.sample_rate)
    }

    /// Duration in milliseconds.
    ///
    /// Prefers the declared frame count over the frames walked. A file that
    /// declares its count is not walked frame by frame at all — the walk would
    /// read every frame to learn a number the header already stated — so
    /// `frames_walked` is 0 there and using it would report a zero-length file.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.trimmed_duration_ms()
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

    // A declared count, if the encoder wrote one. Only Layer III carries a Xing, Info
    // or LAME tag; the other layers put nothing but audio where a tag could go, so
    // looking there would read a coincidental byte sequence as a frame count.
    let declared = if header.layer == Layer::Layer3 {
        let xing = parse_vbr(
            data,
            audio_start.saturating_add(vbr_tag_offset(header.version, header.channels)),
        );
        // VBRI sits at its own fixed distance, so it is a second attempt rather than
        // an alternative to the same offset.
        xing.or_else(|| parse_vbr(data, audio_start.saturating_add(VBRI_TAG_OFFSET)))
    } else {
        None
    };

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
                // The declared byte count covers the header frame, which the frame
                // count does not. Leaving it in makes every bitrate derived from the
                // pair one frame too large, which for a short file is most of the
                // error and for a long one is invisible — the kind of defect that
                // hides in exactly the files nobody checks.
                let first_frame = u64::try_from(header.frame_len()).unwrap_or(0);
                scan.audio_bytes = bytes.saturating_sub(first_frame);
            }
            if let Some(lame) = v.lame {
                let trim = lame.trim_samples();
                let held = scan.samples_of(frames);
                // A trim as long as the audio it describes is not a trim; it is a
                // broken header. Ignoring it keeps the untrimmed count, which is
                // wrong by a few milliseconds rather than empty.
                if trim > 0 && trim < held {
                    scan.encoder_delay_samples = u32::from(lame.delay_samples);
                    scan.encoder_padding_samples = u32::from(lame.padding_samples);
                    scan.encoder_trim_applied = true;
                }
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
    // The walk stopped where frames stopped, and a trailing ID3v1 or APE tag is where
    // frames stop — but it is not audio. `audio_bytes` feeds the CBR duration estimate,
    // so leaving the tag in reports every tagged book long by the tag's size: a few
    // milliseconds on a long file, and a noticeable fraction of a short one.
    if scan.frame_count.is_none() {
        let window_at = data.len().saturating_sub(crate::source::TAIL_WINDOW);
        if let Some(tail) = data.get(window_at..) {
            scan.audio_bytes = scan
                .audio_bytes
                .saturating_sub(crate::source::trailing_tag_bytes(tail).min(scan.audio_bytes));
        }
    }
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
    use alloc::vec;
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

    /// The side-information widths of ISO/IEC 11172-3 table B.1, in bytes, and the
    /// offset each implies once the four-byte frame header is added.
    ///
    /// Written out as a table rather than as four constants so a reader can check the
    /// numbers against the specification instead of against the code below. The widths
    /// here were wrong by one for MPEG-2 for the whole life of the crate, and the test
    /// that "checked" them asserted the code's own numbers, so it agreed.
    const SIDE_INFO: [(MpegVersion, u16, usize, usize); 6] = [
        // version, channels, side info bytes, offset
        (MpegVersion::Mpeg1, 1, 17, 21),
        (MpegVersion::Mpeg1, 2, 32, 36),
        (MpegVersion::Mpeg2, 1, 9, 13),
        (MpegVersion::Mpeg2, 2, 17, 21),
        (MpegVersion::Mpeg25, 1, 9, 13),
        (MpegVersion::Mpeg25, 2, 17, 21),
    ];

    #[test]
    fn the_vbr_offset_table_matches_the_side_information_widths() {
        for (version, channels, side_info, offset) in SIDE_INFO {
            assert_eq!(
                vbr_tag_offset(version, channels),
                4 + side_info,
                "{version:?} {channels}ch: side info is {side_info} bytes"
            );
            assert_eq!(vbr_tag_offset(version, channels), offset);
        }
    }

    /// An `Info` tag as ffmpeg writes it: magic, all four flags, and a LAME
    /// extended header at the end whose version string is ffmpeg's own.
    fn xing_with_lame(frames: u32, bytes: u32, delay: u16, padding: u16) -> Vec<u8> {
        let mut body = b"Info".to_vec();
        body.extend_from_slice(&0xF_u32.to_be_bytes());
        body.extend_from_slice(&frames.to_be_bytes());
        body.extend_from_slice(&bytes.to_be_bytes());
        body.extend_from_slice(&[0x8A; 100]); // seek table
        body.extend_from_slice(&78u32.to_be_bytes()); // quality
                                                      // A LAME header is 24 bytes: a nine-byte version string, then revision and
                                                      // lowpass, peak amplitude, the two gain fields, flags and bitrate, and only
                                                      // then the two counts. The string is nine bytes *exactly* - byte nine is the
                                                      // revision nibble, so a longer string there is read as a revision and the
                                                      // header is rejected, which is the spec's rule and not a detail.
        body.extend_from_slice(b"Lavc58.18");
        body.resize(body.len() + 15, 0);
        let at = body.len() - 3;
        body[at] = (delay >> 4) as u8;
        body[at + 1] = (((delay & 0x0F) << 4) | ((padding >> 8) & 0x0F)) as u8;
        body[at + 2] = (padding & 0xFF) as u8;
        body
    }

    /// A real MPEG-2 mono first frame with `body` sitting where the spec puts an
    /// `Info`/`Xing` tag, inside the frame rather than spilling past it.
    fn mpeg2_mono_frame_with(body: &[u8]) -> Vec<u8> {
        // Index 14 is 144 kbit/s, whose frame is long enough to hold a 144-byte tag
        // body; the bitrate is the tag frame's placeholder and nothing else uses it.
        let header = mpeg2_mono_header(14);
        let frame_len = parse_header(&header).expect("valid header").frame_len();
        let offset = vbr_tag_offset(MpegVersion::Mpeg2, 1);
        assert!(
            offset + body.len() <= frame_len,
            "the fixture's tag must fit inside its own frame, as a real one does"
        );
        let mut data = header.to_vec();
        data.resize(offset, 0);
        data.extend_from_slice(body);
        data.resize(frame_len + 64, 0);
        data
    }

    /// A header whose sync, version, layer, bitrate, rate and mode are all set.
    fn mpeg2_mono_header(bitrate_index: u8) -> [u8; 4] {
        let mut h = [0u8; 4];
        h[0] = 0xFF;
        // MPEG-2 (version bits 10), Layer III (01), no CRC.
        h[1] = 0xE0 | (0b10 << 3) | (0b01 << 1) | 1;
        h[2] = bitrate_index << 4; // sample rate 00 = 22050 Hz, no padding
        h[3] = 0b1100_0000; // mono
        h
    }

    #[test]
    fn a_lame_tag_splits_its_two_counts_out_of_three_bytes() {
        // 576 + 792 is the exact pair ffmpeg wrote into a 10 s 22.05 kHz mono file.
        let tag = xing_with_lame(384, 80_456, 576, 792);
        // The offset is what the table says a real MPEG-2 mono frame holds before the
        // tag; the fixture starts at the tag because the table is checked separately.
        assert_eq!(vbr_tag_offset(MpegVersion::Mpeg2, 1), 13);
        let parsed = parse_vbr(&tag, 0).expect("Info tag");
        let lame = parsed.lame.expect("a LAME header follows");
        assert_eq!(lame.delay_samples, 576);
        assert_eq!(lame.padding_samples, 792);
        assert_eq!(lame.trim_samples(), 1368);
    }

    #[test]
    fn the_encoder_trim_comes_out_of_the_declared_duration() {
        // The real numbers out of a 12 s 22.05 kHz mono file ffmpeg wrote, byte for
        // byte, because they are the numbers ffprobe was checked against: 462 frames
        // of 576 samples is 12.069 s of samples, the encoder admits 576 before and
        // 936 after, and 12.000 s is what is left.
        let data = mpeg2_mono_frame_with(&xing_with_lame(462, 48_456, 576, 936));

        let scan = walk(&data, ContainerKind::Mp3Bare).expect("scans");
        assert!(
            scan.encoder_trim_applied,
            "a LAME tag was present and honoured"
        );
        assert_eq!(scan.encoder_delay_samples, 576);
        assert_eq!(scan.encoder_padding_samples, 936);
        // The untrimmed figure is what a reader that ignores the tag would report -
        // and what mutagen reports for this very file, since it requires the literal
        // string `LAME` and ffmpeg writes `Lavc`.
        assert_eq!(
            scan.samples_from_frames(scan.frame_count.unwrap_or(0)),
            12_068
        );
        assert_eq!(scan.trimmed_duration_ms(), 12_000);
        assert_eq!(scan.duration_ms(), 12_000);
    }

    #[test]
    fn a_trim_as_long_as_the_audio_is_a_broken_tag_and_is_ignored() {
        // Old LAME versions wrote delay and padding on short, low-bitrate files that
        // held no audio at all. Subtracting them reports a zero-length book, so the
        // untrimmed count stands and the flag says the trim was not applied - a
        // duration that is a few milliseconds wrong beats one that is empty.
        let data = mpeg2_mono_frame_with(&xing_with_lame(2, 48, 5_000, 5_000));

        let scan = walk(&data, ContainerKind::Mp3Bare).expect("scans");
        assert!(
            !scan.encoder_trim_applied,
            "the tag was distrusted, not used"
        );
        assert_eq!(scan.encoder_delay_samples, 0);
        assert_eq!(scan.trimmed_duration_ms(), scan.samples_from_frames(2));
        assert!(scan.trimmed_duration_ms() > 0, "never a zero-length book");
    }

    #[test]
    fn every_real_encoder_version_string_is_accepted() {
        // LAME's own name, ffmpeg's muxer and encoder names, and LAME's older one.
        for prefix in [&b"LAME"[..], b"Lavf", b"Lavc", b"L3.99"] {
            let mut body = b"Xing".to_vec();
            body.extend_from_slice(&0x3_u32.to_be_bytes());
            body.extend_from_slice(&10u32.to_be_bytes());
            body.extend_from_slice(&100u32.to_be_bytes());
            body.extend_from_slice(prefix);
            body.resize(body.len() + 9, 0); // complete the nine-byte version string
            body.extend_from_slice(b"3.100");
            body.resize(body.len() + 12, 0);
            let parsed = parse_vbr(&body, 0).expect("Xing tag");
            assert!(
                parsed.lame.is_some(),
                "{:?} is a real encoder string and was rejected",
                core::str::from_utf8(prefix)
            );
        }
    }

    #[test]
    fn something_that_is_not_an_encoder_tag_is_not_parsed_as_one() {
        let mut body = b"Xing".to_vec();
        body.extend_from_slice(&0x3_u32.to_be_bytes());
        body.extend_from_slice(&10u32.to_be_bytes());
        body.extend_from_slice(&100u32.to_be_bytes());
        body.extend_from_slice(b"NOTLAME1.0");
        body.resize(body.len() + 12, 0);
        // 24 bytes of header is what makes this a rejection of the *string* rather
        // than of a truncated read.
        assert!(parse_vbr(&body, 0).expect("Xing tag").lame.is_none());
    }

    #[test]
    fn an_unknown_revision_is_not_read_for_a_delay() {
        // Revision nibble 1 means the layout past this point may differ, so reading
        // twelve bits out of it would be a guess wearing a number's clothes.
        let mut body = b"Xing".to_vec();
        body.extend_from_slice(&0x3_u32.to_be_bytes());
        body.extend_from_slice(&10u32.to_be_bytes());
        body.extend_from_slice(&100u32.to_be_bytes());
        body.extend_from_slice(b"LAME3.100");
        body.resize(body.len() + 12, 0);
        let at = body.len() - 3;
        body[at - 12] = 0x10; // revision 1
        assert!(parse_vbr(&body, 0).expect("Xing tag").lame.is_none());
    }

    #[test]
    fn the_declared_byte_count_excludes_the_header_frame() {
        // Xing's byte count covers the frame the tag lives in, which the frame count
        // does not. Leaving it in makes every bitrate derived from the pair one frame
        // too large - invisible on a long book, most of the error on a short one.
        let body = xing_with_lame(462, 48_456, 0, 0);
        let parsed = parse_vbr(&body, 0).expect("Info tag");
        let bytes = parsed.bytes.expect("byte count");
        let header = mpeg2_mono_header(7); // 56 kbit/s, the placeholder ffmpeg wrote
        let parsed_header = parse_header(&header).expect("valid header");
        assert_eq!(bytes - parsed_header.frame_len() as u64, 48_456 - 182);
    }

    #[test]
    fn a_vbri_tag_is_found_on_mpeg2_where_the_xing_offset_misses_it() {
        // VBRI sits at 36 whatever the version, so a reader that only looks at the
        // Xing offset never sees an MPEG-2 Fraunhofer file's declared frame count.
        assert_eq!(VBRI_TAG_OFFSET, 36);
        let offset = vbr_tag_offset(MpegVersion::Mpeg2, 1);
        assert_ne!(offset, VBRI_TAG_OFFSET);
        let mut body = vec![0u8; VBRI_TAG_OFFSET];
        body.extend_from_slice(b"VBRI");
        body.extend_from_slice(&[0, 1, 0, 0, 0, 0]); // version 1, delay, quality
        body.extend_from_slice(&1_234u32.to_be_bytes()); // bytes
        body.extend_from_slice(&99u32.to_be_bytes()); // frames
        let parsed = parse_vbr(&body, VBRI_TAG_OFFSET).expect("VBRI tag");
        assert_eq!(parsed.frames, Some(99));
        assert_eq!(parsed.bytes, Some(1_234));
        assert_eq!(parsed.is_xing, None);
        // And the Xing offset finds nothing there, which is the whole point.
        assert!(parse_vbr(&body, offset).is_none());
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
            scan.audio_bytes,
            400_000 - 417,
            "the declared size covers the header frame, which the frame count does not"
        );
        // 957 * 1152 = 1_102_464 samples at 44_100 Hz = 24.999 s
        assert_eq!(scan.duration_ms(), 24_999);
        // With both counts known the bitrate is the file's real average, which
        // is near the nominal 128 kbit/s rather than exactly it — the whole point
        // of a VBR file having an average.
        // 399_583 bytes over 24.999 s.
        let implied_bps = scan.audio_bytes * 8 * 1_000 / scan.duration_ms();
        let header = scan.header.expect("a header");
        let nominal = u64::from(header.bitrate_bps);
        assert_eq!(implied_bps, 127_871);
        // The point is that this is the file's *average* and not the rate its frames
        // declare. The direction it falls either side of the nominal rate depends on
        // the encoder, so what is asserted is that the two differ, and by no more than
        // one frame's worth of bytes: that residue is exactly the ambiguity between
        // counting and not counting the header frame, so anything larger would mean
        // the two counts are not describing the same audio.
        assert_ne!(
            implied_bps, nominal,
            "an average equal to the rate is a coincidence"
        );
        let one_frame_of_bytes = 8 * u64::try_from(header.frame_len()).unwrap_or(0);
        let spread = nominal.abs_diff(implied_bps) * scan.duration_ms();
        assert!(
            spread <= one_frame_of_bytes * 1_000,
            "the average differs from the nominal rate by {} bps, more than one frame",
            nominal.abs_diff(implied_bps)
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
