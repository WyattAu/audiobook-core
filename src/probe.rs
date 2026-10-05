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
            chapters: container::mp4_chapters(data),
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

        if let Some(frames) = audio.frame_count {
            let ms = audio.samples_from_frames(frames);
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
