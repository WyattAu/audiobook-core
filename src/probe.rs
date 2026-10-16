//! Probing a file for the numbers a library displays before it plays anything.
//!
//! Duration, sample rate, bitrate, and variable-vs-constant bitrate are the four
//! facts every player shows in its "now playing" panel, and all four are
//! available without decoding a single sample. That is what this module
//! computes.
//!
//! # Why MP3 needs a frame walker
//!
//! An MP4 states its duration in `mvhd`/`mdhd`. An MP3 does not: the format has
//! no duration field anywhere, so a duration is *derived* by walking frame
//! headers. Two sources make that reliable, and both are checked:
//!
//! - **Xing / Info** (LAME, and most encoders) — a header inside the first frame
//!   declaring the total frame count and byte count. For a VBR file this is
//!   exact.
//! - **VBRI** (Fraunhofer) — the same information at a different offset.
//!
//! When neither is present the file is CBR, and the duration follows from the
//! bitrate and the byte count. That derivation is sound because a CBR file's
//! bitrate is constant by definition — so the result is reported with
//! `frame_count: None` rather than being presented as a counted fact.
//!
//! # Honesty rules
//!
//! - Every field that was *counted* is `Some`; every field that was *derived*
//!   is `Some` but the corresponding count is `None`.
//! - [`MediaProbe::confidence`] says which it was, so a caller can prefer a
//!   trusted duration when it has one to compare against.
//!
//! # What this does not do
//!
//! **It does not decode.** No PCM comes out of here, and no sample-accurate seek
//! position is computed — both need a decoder, which the estate does not have.

use alloc::string::String;
use alloc::vec::Vec;

use crate::container::{self, ContainerKind};
use crate::source::{read_head, read_tail, read_window, AUDIO_WINDOW, MAX_TAG_BYTES};

/// How much to trust a reported duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[non_exhaustive]
pub enum Confidence {
    /// Nothing was determinable.
    ///
    /// This is the default, and it must be: a default-constructed probe has
    /// measured nothing, so defaulting it to [`Confidence::Declared`] would let a
    /// caller read a missing duration as a trusted one.
    ///
    /// The declaration order is load-bearing — it is what `Ord` derives from, so
    /// that `Iterator::max` over a set of probes picks the most trustworthy one.
    #[default]
    Unknown,
    /// The number was derived from a bitrate and a byte count. Sound for CBR,
    /// approximate for VBR.
    Derived,
    /// The number came from a declared frame count (Xing/VBRI) or a container
    /// duration field.
    Declared,
}

/// Everything a library can learn about a file without decoding it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaProbe {
    /// What kind of container this is.
    pub container: Option<ContainerKind>,
    /// Duration in milliseconds, if determinable.
    pub duration_ms: Option<u64>,
    /// Audio sample rate in Hz, if determinable.
    pub sample_rate: Option<u32>,
    /// Channel count, if determinable.
    pub channels: Option<u16>,
    /// Overall bitrate in bits per second, if determinable.
    pub bitrate_bps: Option<u64>,
    /// Whether the bitrate varies between frames.
    pub variable_bitrate: Option<bool>,
    /// The exact frame count, when the file declares one.
    ///
    /// `None` for a CBR file, where no such header exists and the frame count is
    /// not a meaningful quantity — it varies with padding bits.
    pub frame_count: Option<u64>,
    /// How much to trust [`MediaProbe::duration_ms`].
    pub confidence: Confidence,
    /// The declared title, if the container carries one.
    pub title: Option<String>,
    /// The declared artist or narrator, if the container carries one.
    pub artist: Option<String>,
    /// The declared album, if the container carries one.
    pub album: Option<String>,
    /// The declared year, if the container carries one.
    pub year: Option<String>,
    /// The declared track number, if the container carries one.
    pub track: Option<u16>,
    /// Cover art bytes, if the container carries any.
    ///
    /// Raw and unvalidated; checking that it decodes is `media-kit`'s job.
    pub cover: Option<Vec<u8>>,
    /// The chapters, already flattened to `(start_ms, title)` pairs.
    pub chapters: Vec<(u64, String)>,
    /// Which chapter mechanisms the container carried, when it carried more than one.
    ///
    /// `None` for the ordinary case. A file with a `chpl` box *and* a chapter track
    /// that disagree is a real condition — a muxer can write each from a different
    /// source — and a library that silently prefers one shows the user a list the file
    /// contradicts. Reporting it costs nothing and lets a caller decide.
    pub chapter_disagreement: Option<ChapterDisagreement>,
    /// Whether the encoder's own delay and padding were taken out of the duration.
    ///
    /// `false` means the duration is the length of the samples the file *holds*, which
    /// for an MP3 is a little longer than the audio in it. Every MP3 has that gap and
    /// most are small, but a caller comparing durations against a player needs to know
    /// whether the two are expected to agree: this flag is the answer, and defaulting it
    /// to `true` would let an MP4 claim a trim that no MP4 has.
    pub encoder_trim_applied: bool,
}

/// A disagreement between a file's two chapter mechanisms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterDisagreement {
    /// Chapters as the Nero `chpl` box states them.
    pub chpl: Vec<(u64, String)>,
    /// Chapters as the QuickTime chapter track states them.
    pub chapter_track: Vec<(u64, String)>,
}

/// Compare a file's two chapter mechanisms, when it has both.
fn chapter_mechanisms(file: &mp4_core::Mp4File) -> Option<ChapterDisagreement> {
    file.chapter_disagreement()
        .map(|(a, b)| ChapterDisagreement {
            chpl: a.iter().map(|c| (c.start_ms, c.title.clone())).collect(),
            chapter_track: b.iter().map(|c| (c.start_ms, c.title.clone())).collect(),
        })
}

impl MediaProbe {
    /// Probe a file's bytes.
    ///
    /// Never fails: bytes it cannot identify yield a probe with
    /// [`Confidence::Unknown`] and no fields set. That is deliberate — a
    /// library should be able to list a folder containing one unreadable file
    /// without failing the whole listing.
    #[must_use]
    pub fn probe(data: &[u8]) -> Self {
        match container::detect(data) {
            Some(ContainerKind::Mp4) => Self::probe_mp4(data),
            Some(ContainerKind::Mp3Id3) => Self::probe_mp3(data),
            Some(ContainerKind::Mp3Bare) => Self::probe_mp3(data),
            None => Self::default(),
        }
    }

    /// Probe a file through bounded reads, for a caller that does not hold the file.
    ///
    /// # What is bounded, and what is not
    ///
    /// An MP3's whole story is in its first few kilobytes — tag, first frame, and the
    /// Xing/Info/LAME headers inside that frame — so the reads are bounded by the tag's
    /// own declared size plus one frame. An MP4's duration, metadata and `chpl` list are
    /// in its `moov`, which `read_top_level` finds by reading box *headers* only, so a
    /// 2 GB `mdat` costs sixteen bytes; the chapter track's samples are then fetched one
    /// bounded read each. No path here holds a whole file, whatever its size.
    ///
    /// Every field agrees with [`MediaProbe::probe`] on the same file — that is asserted
    /// by tests, because a windowed reader that produced a *plausible* answer rather than
    /// the right one would be worse than the slow path it replaces.
    #[must_use]
    pub fn probe_source(source: &mut impl mp4_core::ByteReader) -> Self {
        let head = read_head(source);
        let Some(kind) = container::detect(&head) else {
            return Self::default();
        };
        match kind {
            ContainerKind::Mp4 => Self::probe_mp4_source(source, &head),
            // The head is passed in rather than read again: the windowed path's whole
            // point is what it does *not* read, and reading the same 16 KB twice to save
            // an argument would be a strange way to keep that promise.
            ContainerKind::Mp3Id3 | ContainerKind::Mp3Bare => {
                Self::probe_mp3_source(source, kind, &head)
            }
        }
    }

    /// Probe an MP4 by locating its `moov` and reading that.
    #[must_use]
    fn probe_mp4_source(source: &mut impl mp4_core::ByteReader, head: &[u8]) -> Self {
        let total = source.byte_len().unwrap_or(0);
        // `ftyp` is inside the head window by construction: it is the file's first box.
        let ftyp_payload: Option<alloc::vec::Vec<u8>> =
            head.get(4..8).filter(|t| *t == b"ftyp").and_then(|_| {
                let size = u32::from_be_bytes([
                    *head.first()?,
                    *head.get(1)?,
                    *head.get(2)?,
                    *head.get(3)?,
                ]);
                let size = usize::try_from(size).ok()?;
                head.get(8..size.max(8)).map(<[u8]>::to_vec)
            });

        let Ok(top) = mp4_core::io::read_top_level(source, total) else {
            return Self {
                container: Some(ContainerKind::Mp4),
                ..Self::default()
            };
        };
        let Some(moov_at) = top.iter().find(|b| b.box_type == *b"moov") else {
            return Self {
                container: Some(ContainerKind::Mp4),
                ..Self::default()
            };
        };
        let Ok(moov_bytes) = mp4_core::io::read_box(source, moov_at) else {
            return Self {
                container: Some(ContainerKind::Mp4),
                ..Self::default()
            };
        };
        let Ok(parsed) = mp4_core::container::parse_boxes(&moov_bytes) else {
            return Self {
                container: Some(ContainerKind::Mp4),
                ..Self::default()
            };
        };
        let moov = parsed.first();
        let file = mp4_core::Mp4File::parse_from(ftyp_payload.as_deref(), moov, source);
        let mut probe = Self::from_mp4_file(&file);
        // The sample rate lives in the `stsd` entry inside the moov, so the windowed
        // path reads it there rather than through the whole-file helper.
        if let Some(rate) = moov.and_then(mp4_core::read_sample_rate_in) {
            probe.sample_rate = Some(rate);
        }
        // And the bitrate from the `mdat` sizes the walk already knows, so neither path
        // has to look inside one.
        let mdat: u64 = top
            .iter()
            .filter(|b| b.box_type == *b"mdat")
            .map(|b| b.size.saturating_sub(b.header_len))
            .sum();
        if let (Some(duration_ms), true) = (probe.duration_ms, mdat > 0) {
            probe.bitrate_bps = bitrate_bps(mdat, duration_ms);
        }
        probe
    }

    /// The fields of a summary, from an already-parsed MP4.
    ///
    /// Shared by the whole-file and windowed paths so the two cannot disagree about what
    /// a field *means* — only about where the bytes came from.
    #[must_use]
    fn from_mp4_file(file: &mp4_core::Mp4File) -> Self {
        let probe = Self {
            container: Some(ContainerKind::Mp4),
            duration_ms: file.duration_ms(),
            confidence: if file.duration_ms().is_some() {
                Confidence::Declared
            } else {
                Confidence::Unknown
            },
            title: file.metadata.text(mp4_core::atoms::TITLE).map(String::from),
            artist: file
                .metadata
                .text(mp4_core::atoms::ARTIST)
                .map(String::from),
            album: file.metadata.text(mp4_core::atoms::ALBUM).map(String::from),
            year: file.metadata.text(mp4_core::atoms::YEAR).map(String::from),
            track: file.metadata.track_number(),
            cover: file.metadata.cover().map(<[u8]>::to_vec),
            chapters: file
                .effective_chapters()
                .chapters
                .iter()
                .map(|c| (c.start_ms, c.title.clone()))
                .collect(),
            chapter_disagreement: chapter_mechanisms(file),
            ..Self::default()
        };
        probe
    }

    /// Probe an MP3 through bounded reads.
    ///
    /// The tag is read whole — a chapter frame can sit after the cover art, so half a tag
    /// is worse than no tag — and then one frame's worth of audio, which holds every
    /// header the duration comes from.
    #[must_use]
    fn probe_mp3_source(
        source: &mut impl mp4_core::ByteReader,
        kind: ContainerKind,
        head: &[u8],
    ) -> Self {
        let total = source.byte_len().unwrap_or(0);

        // The tag's own declared size, from its header, decides how much to read. A tag
        // longer than the head window is re-read to its full size; one beyond the cap is
        // read to the cap and the frames past it are absent, not guessed at.
        let tag_len: u64 = if head.get(..3) == Some(b"ID3") {
            let size = id3_core::synchsafe::decode_value([
                *head.get(6).unwrap_or(&0),
                *head.get(7).unwrap_or(&0),
                *head.get(8).unwrap_or(&0),
                *head.get(9).unwrap_or(&0),
            ]);
            (u64::from(size) + 10).min(MAX_TAG_BYTES)
        } else {
            0
        };
        let tag_bytes = read_window(source, 0, usize::try_from(tag_len).unwrap_or(0));
        let audio_start = u64::try_from(tag_bytes.len()).unwrap_or(0);

        let mut probe = Self {
            container: Some(kind),
            ..Self::default()
        };
        if let Ok(tag) = id3_core::Tag::parse(&tag_bytes) {
            probe.title = tag.text_field(id3_core::TextField::Title).map(String::from);
            probe.artist = tag
                .text_field(id3_core::TextField::Artist)
                .map(String::from);
            probe.album = tag.text_field(id3_core::TextField::Album).map(String::from);
            probe.year = tag.text_field(id3_core::TextField::Year).map(String::from);
            probe.track = tag
                .text_field(id3_core::TextField::Track)
                .and_then(parse_leading_number);
            if let Some(pic) = tag.cover() {
                probe.cover = Some(pic.data.clone());
            }
            probe.chapters = container::mp3_chapters(&tag_bytes);
        }

        // One frame's worth of audio, which carries the first frame header and the
        // Xing/Info/LAME headers inside it.
        let audio = read_window(source, audio_start, AUDIO_WINDOW);
        // `Mp3Bare`, not `kind`: the tag has already been read off above, so looking for
        // another one inside the audio window would be the bug that makes every tagged
        // file report nothing. This is exactly what the equivalence gate caught.
        let Some((first, header)) = crate::mpeg::first_frame(&audio, ContainerKind::Mp3Bare) else {
            return probe;
        };
        probe.sample_rate = Some(header.sample_rate);
        probe.channels = Some(header.channels);
        probe.bitrate_bps = Some(u64::from(header.bitrate_bps));

        // A declared count, if the encoder wrote one — the only exact source.
        let vbr_off = crate::mpeg::vbr_tag_offset(header.version, header.channels);
        let declared =
            crate::mpeg::parse_vbr(&audio, first.saturating_add(vbr_off)).or_else(|| {
                crate::mpeg::parse_vbr(&audio, first.saturating_add(crate::mpeg::VBRI_TAG_OFFSET))
            });

        let Some(v) = declared.filter(|v| v.frames.filter(|f| *f > 0).is_some()) else {
            // No declared count: the duration follows from the bitrate and the bytes,
            // which is the same derivation the whole-file path makes — it just does not
            // have to walk every frame to get there. `frame_count` stays `None` so
            // nothing downstream can mistake the estimate for a count.
            probe.variable_bitrate = Some(false);
            let tail = read_tail(source, total);
            let trailing = crate::source::trailing_tag_bytes(&tail);
            let audio_bytes = total.saturating_sub(audio_start).saturating_sub(trailing);
            if header.bitrate_bps > 0 {
                probe.bitrate_bps = Some(u64::from(header.bitrate_bps));
                probe.duration_ms =
                    duration_from_bitrate(audio_bytes, u64::from(header.bitrate_bps));
                probe.confidence = Confidence::Derived;
            }
            return probe;
        };

        let Some(frames) = v.frames.filter(|f| *f > 0) else {
            return probe;
        };
        let mut scan = crate::mpeg::AudioScan {
            header: Some(header),
            frame_count: Some(frames),
            frames_walked: 0,
            // The declared byte count covers the frame the tag lives in, which the frame
            // count does not; where the encoder said nothing, the tail says how much of
            // the file was not audio.
            audio_bytes: match v.bytes.filter(|b| *b > 0) {
                Some(b) => b.saturating_sub(u64::try_from(header.frame_len()).unwrap_or(0)),
                None => {
                    // The tail is read only when a declared byte count is missing,
                    // because its only job here is to say how much of the file was not
                    // audio.
                    let tail = read_tail(source, total);
                    let trailing = crate::source::trailing_tag_bytes(&tail);
                    total.saturating_sub(audio_start).saturating_sub(trailing)
                }
            },
            sample_rate: header.sample_rate,
            channels: header.channels,
            bitrate_bps: u64::from(header.bitrate_bps),
            variable_bitrate: v.is_xing.unwrap_or(true),
            encoder_delay_samples: 0,
            encoder_padding_samples: 0,
            encoder_trim_applied: false,
        };
        if let Some(lame) = v.lame {
            let trim = lame.trim_samples();
            let held = scan.samples_of(frames);
            if trim > 0 && trim < held {
                scan.encoder_delay_samples = u32::from(lame.delay_samples);
                scan.encoder_padding_samples = u32::from(lame.padding_samples);
                scan.encoder_trim_applied = true;
            }
        }
        probe.frame_count = scan.frame_count;
        probe.variable_bitrate = Some(scan.variable_bitrate);
        probe.encoder_trim_applied = scan.encoder_trim_applied;
        probe.duration_ms = Some(scan.trimmed_duration_ms());
        probe.confidence = Confidence::Declared;
        if scan.duration_ms() > 0 {
            probe.bitrate_bps = bitrate_bps(scan.audio_bytes, scan.duration_ms());
        }
        probe
    }

    /// Probe an ISO base media file.
    #[must_use]
    pub fn probe_mp4(data: &[u8]) -> Self {
        let Ok(file) = mp4_core::Mp4File::parse(data) else {
            return Self {
                container: Some(ContainerKind::Mp4),
                ..Self::default()
            };
        };

        let mut probe = Self {
            container: Some(ContainerKind::Mp4),
            duration_ms: file.duration_ms(),
            confidence: if file.duration_ms().is_some() {
                Confidence::Declared
            } else {
                Confidence::Unknown
            },
            title: file.metadata.text(mp4_core::atoms::TITLE).map(String::from),
            artist: file
                .metadata
                .text(mp4_core::atoms::ARTIST)
                .map(String::from),
            album: file.metadata.text(mp4_core::atoms::ALBUM).map(String::from),
            year: file.metadata.text(mp4_core::atoms::YEAR).map(String::from),
            track: file.metadata.track_number(),
            cover: file.metadata.cover().map(<[u8]>::to_vec),
            // The QuickTime chapter track where there is one, falling back to the Nero
            // `chpl` box. Not an arbitrary preference: QuickTime, iTunes and the
            // built-in iOS audiobook player read the track and ignore Nero chapters
            // entirely, so preferring `chpl` would mean preferring the mechanism those
            // players discard. A file carrying both and disagreeing is worth reporting,
            // so the two are also compared.
            // In **file order, not time order**. Sorting here would hide a producer
            // that wrote its chapters backwards, and `Title::validate` exists to report
            // exactly that — a silently corrected list is a defect no consumer can
            // then see. The chapter-track path is naturally monotonic anyway, since its
            // times are accumulated from `stts`.
            chapters: file
                .effective_chapters()
                .chapters
                .iter()
                .map(|c| (c.start_ms, c.title.clone()))
                .collect(),
            chapter_disagreement: chapter_mechanisms(&file),
            ..Self::default()
        };

        probe.sample_rate = mp4_core::moov::read_sample_rate(data);

        // Bitrate from the media-data size: the only container-level byte count
        // available, and it is exact once the `mdat` header is excluded.
        if let (Some(duration_ms), Some(bytes)) = (probe.duration_ms, mdat_size(data)) {
            probe.bitrate_bps = bitrate_bps(bytes, duration_ms);
        }

        probe
    }

    /// Probe an MPEG audio stream, ID3 tag or not.
    #[must_use]
    pub fn probe_mp3(data: &[u8]) -> Self {
        let kind = container::detect(data).unwrap_or(ContainerKind::Mp3Bare);
        let mut probe = Self {
            container: Some(kind),
            ..Self::default()
        };

        // Metadata from the tag, if any. A bare MP3 contributes nothing here.
        if let Ok(tag) = id3_core::Tag::parse(data) {
            probe.title = tag.text_field(id3_core::TextField::Title).map(String::from);
            probe.artist = tag
                .text_field(id3_core::TextField::Artist)
                .map(String::from);
            probe.album = tag.text_field(id3_core::TextField::Album).map(String::from);
            probe.year = tag.text_field(id3_core::TextField::Year).map(String::from);
            probe.track = tag
                .text_field(id3_core::TextField::Track)
                .and_then(parse_leading_number);
            if let Some(pic) = tag.cover() {
                probe.cover = Some(pic.data.clone());
            }
            probe.chapters = container::mp3_chapters(data);
        }

        let audio = match crate::mpeg::walk(data, kind) {
            Some(a) => a,
            None => return probe,
        };

        probe.sample_rate = Some(audio.sample_rate);
        probe.channels = Some(audio.channels);
        probe.frame_count = audio.frame_count;
        probe.variable_bitrate = Some(audio.variable_bitrate);

        if audio.frame_count.is_some() {
            probe.encoder_trim_applied = audio.encoder_trim_applied;
            let ms = audio.trimmed_duration_ms();
            probe.duration_ms = Some(ms);
            probe.confidence = Confidence::Declared;
            // A declared frame count makes the bitrate exact too: total bytes
            // over total time, which is the file's real average bitrate.
            probe.bitrate_bps = bitrate_bps(audio.audio_bytes, ms);
        } else {
            // No count header, so the stream is CBR and the bitrate is constant.
            probe.variable_bitrate = Some(false);
            if audio.bitrate_bps > 0 {
                probe.bitrate_bps = Some(audio.bitrate_bps);
                probe.duration_ms = duration_from_bitrate(audio.audio_bytes, audio.bitrate_bps);
                probe.confidence = Confidence::Derived;
            }
        }

        probe
    }

    /// Whether the file declares any chapters.
    #[must_use]
    pub fn has_chapters(&self) -> bool {
        !self.chapters.is_empty()
    }

    /// The duration formatted as `H:MM:SS` or `M:SS`, the shape a player shows.
    ///
    /// Hours are omitted below one hour because every player omits them there,
    /// and a title's own running time in the UI should match the player's.
    #[must_use]
    pub fn duration_display(&self) -> String {
        let Some(total_s) = self.duration_ms.map(|ms| ms / 1000) else {
            return String::from("--:--");
        };
        let (h, m, s) = (total_s / 3600, (total_s % 3600) / 60, total_s % 60);
        if h > 0 {
            let mut out = String::new();
            out.push_str(&alloc::format!("{h}:"));
            out.push_str(&alloc::format!("{m:02}:"));
            out.push_str(&alloc::format!("{s:02}"));
            out
        } else {
            let mut out = String::new();
            out.push_str(&alloc::format!("{m}:"));
            out.push_str(&alloc::format!("{s:02}"));
            out
        }
    }
}

/// Bits per second from a byte count and a duration, or `None` if either is zero.
///
/// The division is checked rather than guarded: a zero duration is reachable — a
/// container whose `mvhd` carries a timescale but no elapsed time — and an
/// unchecked divide would panic on a file a library is trying to list.
fn bitrate_bps(bytes: u64, duration_ms: u64) -> Option<u64> {
    per_1000_ms(bytes, duration_ms)
}

/// Milliseconds from a byte count and a bitrate, the same relation inverted.
///
/// Named separately rather than reusing [`bitrate_bps`] so a call site reads as
/// what it computes; the shared multiply is factored out so both cannot drift.
fn duration_from_bitrate(bytes: u64, bitrate_bps: u64) -> Option<u64> {
    per_1000_ms(bytes, bitrate_bps)
}

/// `bytes * 8 * 1000 / divisor`, with every step checked.
fn per_1000_ms(bytes: u64, divisor: u64) -> Option<u64> {
    bytes
        .checked_mul(8)
        .and_then(|v| v.checked_mul(1_000))
        .and_then(|v| v.checked_div(divisor))
}

/// Read the first decimal number from a string, if it has one.
///
/// Track fields are frequently `"3/12"` or `"3 of 12"`, so parsing the whole
/// field as an integer would miss most real files.
fn parse_leading_number(s: &str) -> Option<u16> {
    let digits: String = s.trim().chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The payload size of the first `mdat` box, excluding its header.
#[must_use]
pub fn mdat_size(data: &[u8]) -> Option<u64> {
    let mut offset = 0usize;
    while let Some(rest) = data.get(offset..) {
        if rest.len() < 8 {
            return None;
        }
        let size32 = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?);
        let box_type: [u8; 4] = rest.get(4..8)?.try_into().ok()?;
        let (total, header) = match size32 {
            0 => (rest.len() as u64, 8usize),
            1 => {
                let wide: [u8; 8] = rest.get(8..16)?.try_into().ok()?;
                (u64::from_be_bytes(wide), 16usize)
            }
            n => (u64::from(n), 8usize),
        };
        if &box_type == b"mdat" {
            return Some(total.saturating_sub(header as u64));
        }
        let step = usize::try_from(total).unwrap_or(usize::MAX);
        if step == 0 {
            return None;
        }
        offset = offset.saturating_add(step);
    }
    None
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

    #[test]
    fn probe_ordering_prefers_declared() {
        assert!(Confidence::Declared > Confidence::Derived);
        assert!(Confidence::Derived > Confidence::Unknown);
    }

    #[test]
    fn display_omits_hours_when_under_one() {
        let p = MediaProbe {
            duration_ms: Some(65_000),
            ..MediaProbe::default()
        };
        assert_eq!(p.duration_display(), "1:05");

        let p = MediaProbe {
            duration_ms: Some(3_725_000),
            ..MediaProbe::default()
        };
        assert_eq!(p.duration_display(), "1:02:05");

        let p = MediaProbe {
            duration_ms: Some(59_000),
            ..MediaProbe::default()
        };
        assert_eq!(p.duration_display(), "0:59");
    }

    #[test]
    fn display_says_so_when_there_is_no_duration() {
        let p = MediaProbe::default();
        assert_eq!(p.duration_display(), "--:--");
    }

    #[test]
    fn junk_probes_to_unknown_rather_than_failing() {
        let p = MediaProbe::probe(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(p.container, None);
        assert_eq!(p.confidence, Confidence::Unknown);
        assert_eq!(p.duration_ms, None);
    }

    #[test]
    fn an_empty_file_probes_to_unknown() {
        assert_eq!(MediaProbe::probe(&[]).confidence, Confidence::Unknown);
    }

    #[test]
    fn a_leading_track_number_is_read_from_slash_notation() {
        assert_eq!(parse_leading_number("3/12"), Some(3));
        assert_eq!(parse_leading_number(" 7 "), Some(7));
        assert_eq!(parse_leading_number("A1"), None);
        assert_eq!(parse_leading_number(""), None);
    }

    #[test]
    fn mdat_size_excludes_the_header() {
        // An 8-byte header plus 100 bytes of payload.
        let mut data = 108u32.to_be_bytes().to_vec();
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&[0u8; 100]);
        assert_eq!(mdat_size(&data), Some(100));
    }

    #[test]
    fn mdat_size_is_none_when_there_is_no_mdat() {
        let mut data = 12u32.to_be_bytes().to_vec();
        data.extend_from_slice(b"ftyp");
        data.extend_from_slice(&[0u8; 4]);
        assert_eq!(mdat_size(&data), None);
        assert_eq!(mdat_size(&[]), None);
        // Declares a size far beyond the buffer: the walk stops rather than
        // looping forever.
        assert_eq!(mdat_size(&[0u8; 8]), None);
    }

    #[test]
    fn an_mp4_duration_is_reported_as_declared() {
        let mvhd = mp4_core::container::leaf(
            *b"mvhd",
            mp4_core::moov::MovieHeader::to_payload(1_000, 3_600_000),
        )
        .expect("infallible");
        let moov = mp4_core::container::container(*b"moov", vec![mvhd]);
        let mut data =
            mp4_core::container::leaf(*b"ftyp", mp4_core::FileType::audiobook().to_payload())
                .expect("infallible")
                .to_bytes();
        data.extend_from_slice(&moov.to_bytes());

        let p = MediaProbe::probe(&data);
        assert_eq!(p.container, Some(ContainerKind::Mp4));
        assert_eq!(p.duration_ms, Some(3_600_000));
        assert_eq!(p.confidence, Confidence::Declared);
        assert_eq!(p.duration_display(), "1:00:00");
    }

    #[test]
    fn a_zero_denominator_yields_nothing_rather_than_a_panic() {
        // Reachable: a container with a timescale and no elapsed time.
        assert_eq!(bitrate_bps(1_000, 0), None);
        assert_eq!(bitrate_bps(1_000, 1_000), Some(8_000));
        assert_eq!(duration_from_bitrate(1_000, 0), None);
        assert_eq!(duration_from_bitrate(1_000, 8_000), Some(1_000));

        // A byte count large enough to overflow the intermediate product is
        // reported as absent rather than wrapping to a small plausible number.
        assert_eq!(bitrate_bps(u64::MAX, 1_000), None);
        assert_eq!(duration_from_bitrate(u64::MAX, 1_000), None);
    }

    #[test]
    fn has_chapters_reflects_the_list() {
        let mut p = MediaProbe::default();
        assert!(!p.has_chapters());
        p.chapters.push((0, String::from("One")));
        assert!(p.has_chapters());
    }
}
