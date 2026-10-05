//! Container detection — telling MP3 from MP4 before trying to read either.
//!
//! Both formats start with bytes that could plausibly be anything, so detection
//! is by explicit signature rather than by extension. An M4B in particular is
//! frequently named `.mp3` after a bad download, and a library that trusts the
//! extension produces an empty title rather than an error.

use alloc::string::String;
use alloc::vec::Vec;

use id3_core::ID3_IDENTIFIER;

/// What kind of file these bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContainerKind {
    /// An MPEG audio stream carrying an ID3v2 tag.
    Mp3Id3,
    /// An MPEG audio stream with no ID3 tag — the tag may be at the end as v1.
    Mp3Bare,
    /// An ISO base media file (MP4/M4A/M4B).
    Mp4,
}

impl ContainerKind {
    /// Whether this container can carry chapters.
    #[must_use]
    pub fn carries_chapters(self) -> bool {
        matches!(self, ContainerKind::Mp3Id3 | ContainerKind::Mp4)
    }

    /// Whether this container can carry cover art.
    #[must_use]
    pub fn carries_art(self) -> bool {
        matches!(self, ContainerKind::Mp3Id3 | ContainerKind::Mp4)
    }

    /// Whether chapters in this container are authoritative, or merely
    /// per-file and needing assembly.
    ///
    /// A single M4B's chapters are one list for the whole book. A folder of MP3s
    /// has one list *per file*, which a caller must concatenate — this crate
    /// reports that difference rather than hiding it.
    #[must_use]
    pub fn chapters_are_per_file(self) -> bool {
        matches!(self, ContainerKind::Mp3Id3 | ContainerKind::Mp3Bare)
    }
}

impl core::fmt::Display for ContainerKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ContainerKind::Mp3Id3 => f.write_str("MP3 with ID3v2"),
            ContainerKind::Mp3Bare => f.write_str("bare MP3"),
            ContainerKind::Mp4 => f.write_str("MP4/M4A/M4B"),
        }
    }
}

/// Detect the container from a file's leading bytes.
///
/// Returns `None` for anything that is not recognisably audio, which the caller
/// should treat as "not mine" rather than "empty book".
#[must_use]
pub fn detect(data: &[u8]) -> Option<ContainerKind> {
    // ID3v2: the literal "ID3" at offset 0.
    if data.get(..3) == Some(ID3_IDENTIFIER.as_slice()) {
        return Some(ContainerKind::Mp3Id3);
    }

    // ID3v1: the literal "TAG" in the last 128 bytes.
    if data.len() >= 128 && data.get(data.len() - 128..data.len() - 125) == Some(b"TAG") {
        return Some(ContainerKind::Mp3Bare);
    }

    // ISO-BMFF: a box size followed by a plausible four-character type. The
    // type must be alphabetic or one of the known printable codes, which is what
    // keeps arbitrary binary from matching.
    if let Some(head) = data.get(4..8) {
        if let Some(kind) = box_type_kind(head) {
            return Some(kind);
        }
    }

    // A bare MPEG audio frame. Not necessarily at offset 0: a file may carry
    // leading junk, an APE tag, or a truncated ID3 header that the strict check
    // above rejects. Scanning a bounded window is what a real library has to do,
    // and the window is bounded because this runs on every file in a listing.
    if find_frame_sync(data).is_some() {
        return Some(ContainerKind::Mp3Bare);
    }

    None
}

/// How far into a file to look for an MPEG frame sync.
///
/// A generous window covers every real prologue — an APE tag, an ID3v1 trailer, a
/// few hundred bytes of padding — while bounding the cost of scanning a
/// non-audio file that happens to be large.
const SYNC_SCAN_BYTES: usize = 8_192;

/// The offset of the first valid MPEG frame header within the scan window.
fn find_frame_sync(data: &[u8]) -> Option<usize> {
    let limit = data.len().min(SYNC_SCAN_BYTES);
    for offset in 0..limit.saturating_sub(3) {
        if crate::mpeg::parse_header(data.get(offset..)?).is_some() {
            return Some(offset);
        }
    }
    None
}

/// Map a four-character box type to a container kind.
fn box_type_kind(head: &[u8]) -> Option<ContainerKind> {
    let printable = head.iter().all(|b| b.is_ascii_alphanumeric() || *b == b' ');
    if !printable {
        return None;
    }
    match head {
        b"ftyp" | b"moov" | b"styp" | b"moof" | b"free" | b"skip" | b"mdat" => {
            Some(ContainerKind::Mp4)
        }
        _ => None,
    }
}

/// Read a title string from an MP3's ID3 tag.
///
/// Returns `None` when the bytes are not an MP3 with an ID3 tag, or the tag has
/// no title — the two are different outcomes and the caller may want to retry
/// with the file's own name.
#[must_use]
pub fn mp3_title(data: &[u8]) -> Option<String> {
    let tag = id3_core::Tag::parse(data).ok()?;
    let title = tag.text_field(id3_core::TextField::Title)?;
    if title.is_empty() {
        None
    } else {
        Some(String::from(title))
    }
}

/// Read a title string from an MP4's iTunes metadata.
#[must_use]
pub fn mp4_title(data: &[u8]) -> Option<String> {
    let file = mp4_core::Mp4File::parse(data).ok()?;
    let t = file.metadata.text(mp4_core::atoms::TITLE)?;
    if t.is_empty() {
        None
    } else {
        Some(String::from(t))
    }
}

/// Read the chapter list from an MP3's ID3 `CHAP`/`CTOC` frames.
#[must_use]
pub fn mp3_chapters(data: &[u8]) -> Vec<(u64, String)> {
    let Ok(tag) = id3_core::Tag::parse(data) else {
        return Vec::new();
    };
    let Ok(resolved) = tag.resolved_chapters() else {
        return Vec::new();
    };
    resolved
        .into_iter()
        .map(|c| {
            // `ResolvedChapter` stores u32 milliseconds; the domain model uses
            // u64 so a concatenation of many long files cannot overflow.
            let title = c.title.unwrap_or_default();
            (u64::from(c.start_ms), title)
        })
        .collect()
}

/// Read the chapter list from an MP4's `chpl` box.
#[must_use]
pub fn mp4_chapters(data: &[u8]) -> Vec<(u64, String)> {
    let Ok(file) = mp4_core::Mp4File::parse(data) else {
        return Vec::new();
    };
    file.chapters
        .sorted()
        .into_iter()
        .map(|c| (c.start_ms, c.title.clone()))
        .collect()
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
    fn detects_an_id3_mp3() {
        let mut data = b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec();
        data.extend_from_slice(&[0u8; 64]);
        assert_eq!(detect(&data), Some(ContainerKind::Mp3Id3));
    }

    #[test]
    fn detects_an_mp4() {
        let mut data = 0u32.to_be_bytes().to_vec();
        data.extend_from_slice(b"ftyp");
        data.extend_from_slice(b"M4B ");
        assert_eq!(detect(&data), Some(ContainerKind::Mp4));
    }

    #[test]
    fn detects_a_bare_mpeg_frame() {
        // 0xFF 0xFB: frame sync, MPEG-1 Layer III.
        let data = vec![0xFF, 0xFB, 0x90, 0x00];
        assert_eq!(detect(&data), Some(ContainerKind::Mp3Bare));
    }

    #[test]
    fn detects_a_frame_that_does_not_start_at_offset_zero() {
        // 300 bytes of leading junk — an APE tag, a bad download, a stale header —
        // must not make the file undetectable.
        let mut data = vec![0u8; 300];
        data.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        assert_eq!(detect(&data), Some(ContainerKind::Mp3Bare));
    }

    #[test]
    fn detection_does_not_scan_the_whole_file() {
        // The scan is bounded, so a large non-audio file with a sync buried deep
        // is not a false positive — and detection stays cheap on a listing.
        let mut data = vec![0u8; 32_768];
        data[20_000..20_004].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        assert_eq!(detect(&data), None);
    }

    #[test]
    fn detects_an_id3v1_trailer() {
        // An ID3v1 tag occupies the *last* 128 bytes and begins with `TAG`, so a
        // literal `TAG` earlier in the file is not one.
        let mut data = vec![0u8; 125];
        data.extend_from_slice(b"TAG");
        data.extend_from_slice(&[0u8; 125]);
        assert_eq!(data.len(), 253, "125 audio bytes then a 128-byte trailer");
        assert_eq!(detect(&data), Some(ContainerKind::Mp3Bare));

        let mut early = b"TAG".to_vec();
        early.extend_from_slice(&[0u8; 128]);
        assert_eq!(
            detect(&early),
            None,
            "a `TAG` at offset 0 is not a trailing ID3v1 tag"
        );
    }

    #[test]
    fn rejects_arbitrary_bytes() {
        assert_eq!(detect(&[]), None);
        assert_eq!(detect(&[0, 1, 2, 3]), None);
        // A PDF is not audio.
        assert_eq!(detect(b"%PDF-1.7\n%\xE2\xE3"), None);
        // A box type with non-printable bytes must not match.
        let mut data = 0u32.to_be_bytes().to_vec();
        data.extend_from_slice(&[0x00, 0x01, 0x02, 0x03]);
        assert_eq!(detect(&data), None);
    }

    #[test]
    fn container_capabilities_are_distinct() {
        assert!(ContainerKind::Mp4.carries_chapters());
        assert!(ContainerKind::Mp3Id3.carries_chapters());
        assert!(
            !ContainerKind::Mp3Bare.carries_chapters(),
            "a bare MP3 has no tag at all"
        );
        assert!(ContainerKind::Mp4.carries_art());
        assert!(ContainerKind::Mp3Id3.chapters_are_per_file());
        assert!(
            !ContainerKind::Mp4.chapters_are_per_file(),
            "an M4B is one file"
        );
    }

    #[test]
    fn display_is_readable() {
        use alloc::string::ToString;
        assert_eq!(ContainerKind::Mp4.to_string(), "MP4/M4A/M4B");
        assert_eq!(ContainerKind::Mp3Id3.to_string(), "MP3 with ID3v2");
    }

    #[test]
    fn reading_helpers_return_none_on_junk() {
        assert_eq!(mp3_title(&[0u8; 4]), None);
        assert_eq!(mp4_title(&[0u8; 4]), None);
        assert!(mp3_chapters(&[0u8; 4]).is_empty());
        assert!(mp4_chapters(&[0u8; 4]).is_empty());
    }
}
