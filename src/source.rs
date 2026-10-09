//! Bounded reads: the pieces a probe needs without holding a file.
//!
//! # Why this exists
//!
//! [`MediaProbe::probe`](crate::MediaProbe::probe) takes a `&[u8]`, which is the right
//! shape for a caller that already has the bytes and the wrong one for a caller with a
//! 700 MB audiobook on disk. Reading a whole file to learn its length — when the length
//! is a frame count in a header near the front, or a duration in a `moov` box at one end
//! or the other — is the difference between listing a library in milliseconds and listing
//! it in minutes.
//!
//! The random-access contract itself is [`mp4_core::ByteReader`], defined at L0 because
//! both container layers need it. Defining it twice — once here, once there — is how two
//! contracts drift apart, so this module only adds what is specific to probing: the
//! window sizes, and the trailing-tag arithmetic.
//!
//! # What a bounded read costs
//!
//! For an MP3 whose encoder wrote a frame count, nothing: the duration is exact and the
//! bytes never leave the header. For one that did not, the count is *not known* and the
//! probe says so — [`Confidence::Derived`](crate::Confidence::Derived) with no frame
//! count, which is what it reports when the whole file is in hand too. A windowed reader
//! that reported a smaller duration because it ran out of bytes would be worse than the
//! slow one, so the windows are sized from the container rather than from a guess.

use alloc::vec::Vec;

/// Bytes read from the start of a file to identify it and find its first frame.
pub const HEAD_WINDOW: usize = 16 * 1024;

/// Bytes read from the start of an MP3's audio, once the tag has been passed, to find the
/// first frame and the Xing, Info and LAME headers inside it.
///
/// A tag frame is at most 417 bytes at the common audiobook rates, and everything the
/// duration needs sits inside it, so 4 KiB is generous.
pub const AUDIO_WINDOW: usize = 4 * 1024;

/// Bytes read from the end of a file.
///
/// Enough to hold an ID3v1 tag and an APE tag footer, which is all that is ever found
/// there. Nothing else needs the tail: an encoder that wrote a frame count puts it at the
/// front, and one that did not is estimated from the bitrate, where the tail's only job
/// is to say how much of the file is *not* audio.
pub const TAIL_WINDOW: usize = 8 * 1024;

/// The largest ID3v2 tag this module reads into memory in one piece.
///
/// A tag with embedded cover art runs to a few megabytes and must be read whole, because
/// a chapter frame can sit after the picture. The cap exists so a corrupt or hostile size
/// field cannot make a library scan allocate whatever it asks for. A tag beyond it is
/// read up to the cap and parsed from that, which for an over-sized tag means the frames
/// after the cap are simply absent — reported as absent, not guessed at.
pub const MAX_TAG_BYTES: u64 = 32 * 1024 * 1024;

/// Read up to `len` bytes from `offset`, returning however many were available.
#[must_use]
pub fn read_window(source: &mut impl mp4_core::ByteReader, offset: u64, len: usize) -> Vec<u8> {
    let mut buf = alloc::vec![0u8; len];
    let read = source.read_at(offset, &mut buf).unwrap_or(0);
    buf.truncate(read);
    buf
}

/// Read the head window: the first [`HEAD_WINDOW`] bytes, or the whole file if it is
/// shorter.
#[must_use]
pub fn read_head(source: &mut impl mp4_core::ByteReader) -> Vec<u8> {
    read_window(source, 0, HEAD_WINDOW)
}

/// Read the tail window: the last [`TAIL_WINDOW`] bytes, or the whole file if it is
/// shorter.
///
/// A file shorter than the window is returned whole rather than twice: a caller asking
/// for the tail of a 2 KB file wants the tail of it, and handing back the head as well
/// would make that file's "last 128 bytes" read as its first.
#[must_use]
pub fn read_tail(source: &mut impl mp4_core::ByteReader, total_len: u64) -> Vec<u8> {
    let window = u64::try_from(TAIL_WINDOW).unwrap_or(u64::MAX);
    if total_len <= window {
        return read_window(source, 0, usize::try_from(total_len).unwrap_or(0));
    }
    read_window(source, total_len - window, TAIL_WINDOW)
}

/// How many bytes of trailing tags sit at the end of `tail`, the last [`TAIL_WINDOW`]
/// bytes of a file.
///
/// Two tags, in the order they are actually written: an APE tag first and the 128-byte
/// ID3v1 tag last, which is what every tagger that writes both does. Both are counted
/// because both are padding masquerading as audio — left in, they make a constant-bitrate
/// duration long by however much they add, which is a rounding error on a long book and
/// a third of the length of a short one.
///
/// Only the end of the file is examined, so a tag followed by more audio is not found.
/// That is not a gap: neither tag is legal anywhere but the end.
#[must_use]
pub fn trailing_tag_bytes(tail: &[u8]) -> u64 {
    // ID3v1 is 128 bytes ending in "TAG", and when both are present it is the outer one.
    let id3v1 = if tail.len() >= 128 && tail.get(tail.len() - 128..tail.len() - 125) == Some(b"TAG")
    {
        128u64
    } else {
        0
    };
    let inner = tail
        .len()
        .saturating_sub(usize::try_from(id3v1).unwrap_or(0));
    if inner < 32 {
        return id3v1;
    }
    let marker_at = inner - 8;
    if tail.get(marker_at..marker_at + 8) != Some(b"APETAGEX") {
        return id3v1;
    }
    let size_at = inner - 32;
    let size = match tail.get(size_at..size_at + 4).and_then(|b| {
        let four = [b.first()?, b.get(1)?, b.get(2)?, b.get(3)?];
        Some(u32::from_le_bytes([*four[0], *four[1], *four[2], *four[3]]))
    }) {
        Some(bytes) => u64::from(bytes),
        None => return id3v1,
    };
    let total = size.saturating_add(32);
    if u64::try_from(inner).unwrap_or(0) < total {
        // A size field larger than the window means the footer is not the end of this
        // file's tags, so nothing is claimed rather than a number nobody can check.
        return id3v1;
    }
    id3v1 + total
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;
    use alloc::vec;
    use mp4_core::ByteReader as _;

    /// A source over a fixed buffer that counts how many bytes were *requested*, so a
    /// test can assert on the volume of I/O and not only on the answer.
    struct Counting {
        data: Vec<u8>,
        read: usize,
    }

    impl Counting {
        fn new(data: Vec<u8>) -> Self {
            Self { data, read: 0 }
        }
    }

    impl mp4_core::ByteReader for Counting {
        fn byte_len(&self) -> Option<u64> {
            Some(u64::try_from(self.data.len()).unwrap_or(u64::MAX))
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Option<usize> {
            self.read += buf.len();
            let mut data: &[u8] = &self.data;
            data.read_at(offset, buf)
        }
    }

    #[test]
    fn a_short_read_at_the_end_of_a_file_is_normal() {
        // The `&[u8]` impl lives in mp4-core, where the contract is defined; this test is
        // that the shape this module assumes — short read, not error — is what it does.
        let mut data: &[u8] = &[1, 2, 3];
        let mut buf = [0u8; 8];
        assert_eq!(data.read_at(0, &mut buf), Some(3));
        assert_eq!(&buf[..3], &[1, 2, 3]);
        assert_eq!(
            data.read_at(3, &mut buf),
            Some(0),
            "past the end is empty, not an error"
        );
        assert_eq!(data.byte_len(), Some(3));
    }

    #[test]
    fn the_head_window_is_capped_so_a_large_file_is_not_read_whole() {
        let big = vec![0xFFu8; 4 * HEAD_WINDOW];
        let mut source = Counting::new(big);
        let head = read_head(&mut source);
        assert_eq!(head.len(), HEAD_WINDOW);
        assert_eq!(source.read, HEAD_WINDOW, "no more than the window");
    }

    #[test]
    fn the_tail_window_reads_the_end_not_the_start() {
        let mut data = vec![0u8; 100];
        data[90..].copy_from_slice(&[7, 7, 7, 7, 7, 7, 7, 7, 7, 7]);
        let total = u64::try_from(data.len()).unwrap_or(0);
        let mut source = Counting::new(data);
        let tail = read_tail(&mut source, total);
        assert_eq!(tail.len(), TAIL_WINDOW.min(100));
        assert_eq!(tail.last(), Some(&7));
    }

    #[test]
    fn a_tail_window_over_a_short_file_is_the_whole_file_not_the_head() {
        let data = vec![1u8, 2, 3, 4];
        let total = u64::try_from(data.len()).unwrap_or(0);
        let mut source = Counting::new(data);
        let tail = read_tail(&mut source, total);
        assert_eq!(tail, vec![1, 2, 3, 4]);
    }

    #[test]
    fn an_id3v1_tag_at_the_end_is_counted() {
        let mut file = vec![0u8; 4096];
        let at = file.len() - 128;
        file[at..at + 3].copy_from_slice(b"TAG");
        assert_eq!(trailing_tag_bytes(&file), 128);
    }

    #[test]
    fn a_file_with_no_trailing_tag_reports_none() {
        let file = vec![0u8; 4096];
        assert_eq!(trailing_tag_bytes(&file), 0);
    }

    #[test]
    fn an_ape_tag_at_the_end_is_counted_by_its_footer() {
        // A 32-byte footer whose size field counts everything but header and footer.
        let size = 64u32;
        let mut file = vec![0u8; 4096];
        let at = file.len() - 32;
        let marker = file.len() - 8;
        file[at..at + 4].copy_from_slice(&size.to_le_bytes());
        file[marker..].copy_from_slice(b"APETAGEX");
        assert_eq!(trailing_tag_bytes(&file), 96);
    }

    #[test]
    fn both_trailing_tags_are_counted_with_id3v1_outermost() {
        // APE first, ID3v1 last, which is the order a tagger writing both uses.
        let size = 64u32;
        let mut file = vec![0u8; 4096];
        let inner_end = file.len() - 128;
        let last = file.len();
        file[inner_end - 32..inner_end - 28].copy_from_slice(&size.to_le_bytes());
        file[inner_end - 8..inner_end].copy_from_slice(b"APETAGEX");
        file[last - 128..last - 125].copy_from_slice(b"TAG");
        assert_eq!(trailing_tag_bytes(&file), 96 + 128);
    }

    #[test]
    fn an_ape_size_bigger_than_the_window_is_not_claimed() {
        // A footer claiming 100 MB from a window that holds 8 KB is not a footer at the
        // end of this file's tags, and reporting it would be a number nobody can check.
        let size = 100u32 * 1024 * 1024;
        let mut file = vec![0u8; 4096];
        let at = file.len() - 32;
        let marker = file.len() - 8;
        file[at..at + 4].copy_from_slice(&size.to_le_bytes());
        file[marker..].copy_from_slice(b"APETAGEX");
        assert_eq!(trailing_tag_bytes(&file), 0);
    }
}
