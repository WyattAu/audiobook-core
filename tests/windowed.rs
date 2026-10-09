//! The windowed probe must agree with the whole-file probe, field for field.
//!
//! These are the gates that make bounded reading worth having. A window that was one byte
//! short — as the MPEG-2 side-information width was for the whole life of the crate —
//! produces not an error but a *plausible wrong answer*: a duration estimated from a
//! placeholder bitrate, a chapter list reported as absent. Agreement with
//! [`audiobook_core::MediaProbe::probe`] on the same bytes is the only check that means
//! anything, and the volume of I/O is asserted alongside it, because an unbounded
//! "windowed" reader would pass every field check while being the thing it exists not to
//! be.
//!
//! Real files — the ones ffmpeg wrote, whose QuickTime chapter samples live in `mdat` —
//! are checked by the conformance harness, which is where the oracle lives. This file
//! checks the arithmetic on fixtures whose every byte is known.

// Fixtures are allowed to assert and to index: an `expect` or a known-good index here is
// the test stating what it assumes, which is the honest form, and the crate lint denies
// both everywhere a caller would write them.
#![allow(clippy::expect_used, clippy::indexing_slicing)]

use audiobook_core::source::{read_head, AUDIO_WINDOW};
use audiobook_core::{Confidence, ContainerKind, MediaProbe};
use mp4_core::ByteReader;

/// A reader over a fixed buffer that counts what was *requested*.
struct Counting {
    data: Vec<u8>,
    read: usize,
}

impl Counting {
    fn new(data: Vec<u8>) -> Self {
        Self { data, read: 0 }
    }
}

impl ByteReader for Counting {
    fn byte_len(&self) -> Option<u64> {
        Some(u64::try_from(self.data.len()).unwrap_or(u64::MAX))
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Option<usize> {
        self.read += buf.len();
        let mut data: &[u8] = &self.data;
        data.read_at(offset, buf)
    }
}

/// Every field a caller can act on, compared when both sides had it.
fn assert_agree(whole: &MediaProbe, windowed: &MediaProbe) {
    assert_eq!(whole.container, windowed.container, "container");
    assert_eq!(whole.duration_ms, windowed.duration_ms, "duration");
    assert_eq!(whole.sample_rate, windowed.sample_rate, "sample rate");
    assert_eq!(whole.channels, windowed.channels, "channels");
    assert_eq!(whole.bitrate_bps, windowed.bitrate_bps, "bitrate");
    assert_eq!(
        whole.variable_bitrate, windowed.variable_bitrate,
        "variable bitrate"
    );
    assert_eq!(whole.frame_count, windowed.frame_count, "frame count");
    assert_eq!(whole.confidence, windowed.confidence, "confidence");
    assert_eq!(whole.title, windowed.title, "title");
    assert_eq!(whole.artist, windowed.artist, "artist");
    assert_eq!(whole.album, windowed.album, "album");
    assert_eq!(whole.year, windowed.year, "year");
    assert_eq!(whole.track, windowed.track, "track");
    assert_eq!(
        whole.encoder_trim_applied, windowed.encoder_trim_applied,
        "whether the encoder trim was applied"
    );
    assert_eq!(
        whole.chapters, windowed.chapters,
        "chapters are the reason a library exists"
    );
    assert_eq!(whole.cover.is_some(), windowed.cover.is_some(), "cover art");
}

/// Probe both ways and return the two, so a test can assert beyond the shared fields.
fn both(data: &[u8]) -> (MediaProbe, MediaProbe, Counting) {
    let whole = MediaProbe::probe(data);
    let mut source = Counting::new(data.to_vec());
    let windowed = MediaProbe::probe_source(&mut source);
    assert_agree(&whole, &windowed);
    (whole, windowed, source)
}

/// An MPEG-2 mono 22.05 kHz MP3 at 32 kbit/s: the encoding a spoken-word book ships in,
/// and the one whose side-information width is easiest to get wrong by one.
const SPOKEN_HEADER: [u8; 4] = [0xFF, 0xF3, 0x40, 0xC4];
const SPOKEN_FRAME_LEN: usize = 104;

/// An MP3 the way ffmpeg writes one: ID3v2 tag, then a tag frame carrying Info and LAME,
/// then the audio.
///
/// The tag frame is written at a *placeholder* bitrate — 56 kbit/s against 32 kbit/s of
/// real audio — because that is what ffmpeg does: the tag will not fit in a 32 kbit/s
/// frame, so the frame that carries it is written longer. A reader that took the first
/// frame's bitrate for the stream's would report every such file a third too slow, which
/// is the fallback this fixture exists to catch.
fn mp3_with_lame(frames: u32, delay: u16, padding: u16) -> Vec<u8> {
    /// The tag frame: 56 kbit/s so the tag fits, which is the placeholder's whole job.
    const TAG_FRAME_LEN: usize = 72 * 56_000 / 22_050; // 182

    let mut data = b"ID3\x04\x00\x00\x00\x00\x00\x22".to_vec();
    data.extend_from_slice(b"TSSE");
    data.extend_from_slice(&14u32.to_be_bytes());
    data.extend_from_slice(&[0u8, 0, 3]);
    data.extend_from_slice(b"Lavf63.1.102");
    data.resize(44, 0);

    // The tag frame: header, MPEG-2 mono side information, then an Info tag with all four
    // flags and a LAME header at the end.
    let mut first = [0xFF, 0xF3, 0x70, 0xC0].to_vec();
    first.extend_from_slice(&[0u8; 9]);
    first.extend_from_slice(b"Info");
    first.extend_from_slice(&0xF_u32.to_be_bytes());
    first.extend_from_slice(&frames.to_be_bytes());
    let declared_bytes = frames * SPOKEN_FRAME_LEN as u32;
    first.extend_from_slice(&declared_bytes.to_be_bytes());
    first.extend_from_slice(&[0x8A; 100]); // seek table
    first.extend_from_slice(&78u32.to_be_bytes()); // quality
                                                   // Nine bytes exactly: the ninth is the revision nibble, so a longer string is read
                                                   // as a revision and the header is refused. The two counts then sit twenty-one bytes
                                                   // past the string's start, packed twelve bits each into three bytes.
    let version_at = first.len();
    first.extend_from_slice(b"Lavc58.18");
    first.resize(version_at + 24, 0);
    first[version_at + 21] = (delay >> 4) as u8;
    first[version_at + 22] = (((delay & 0x0F) << 4) | ((padding >> 8) & 0x0F)) as u8;
    first[version_at + 23] = (padding & 0xFF) as u8;
    assert!(
        first.len() <= TAG_FRAME_LEN,
        "the tag must fit inside the frame that carries it, as a real one does"
    );
    first.resize(TAG_FRAME_LEN, 0);
    data.extend_from_slice(&first);
    for _ in 0..frames {
        data.extend_from_slice(&SPOKEN_HEADER);
        data.resize(data.len() + SPOKEN_FRAME_LEN - 4, 0);
    }
    data
}

#[test]
fn an_mp3_with_a_lame_tag_agrees_whether_read_whole_or_windowed() {
    // 462 frames, 576 before and 936 after: the numbers out of a real 12 s file that
    // ffprobe was checked against, and which the trim arithmetic lands on exactly.
    let data = mp3_with_lame(462, 576, 936);
    let whole = MediaProbe::probe(&data);
    // 462 frames of 576 samples at 22.05 kHz is 12.069 s of samples; 576 before and 936
    // after come out, and 12.000 s is what ffprobe reports for the real file.
    assert_eq!(
        whole.duration_ms,
        Some(12_000),
        "the fixture is the real shape"
    );
    assert!(
        whole.encoder_trim_applied,
        "the LAME header is present and honoured"
    );
    assert_eq!(
        whole.variable_bitrate,
        Some(false),
        "an Info tag means CBR, and the placeholder frame must not change that"
    );
    let (_whole, _windowed, source) = both(&data);
    // Three reads: the head window every probe takes to identify the file, the tag at
    // its own declared size, and one frame of audio. Nothing scales with the length of
    // the book, which is the property the window exists for.
    let expected = audiobook_core::source::HEAD_WINDOW + AUDIO_WINDOW + 44;
    assert_eq!(
        source.read,
        expected,
        "read {} bytes of {}",
        source.read,
        462 * SPOKEN_FRAME_LEN
    );
}

#[test]
fn a_cbr_mp3_without_a_declared_count_agrees_too() {
    // No Xing/Info anywhere. The whole-file path walks every frame; the windowed one
    // derives the duration from the bitrate and the byte count. They agree because a
    // constant bitrate is constant and both know the audio's byte count — and the
    // confidence both report is Derived, which is what says it.
    let header: [u8; 4] = [0xFF, 0xFB, 0x50, 0xC0]; // MPEG-1 Layer III 64k mono 44.1k
    let frame_len = 144 * 64_000 / 44_100;
    let mut data = Vec::new();
    for _ in 0..480 {
        data.extend_from_slice(&header);
        data.resize(data.len() + frame_len - 4, 0);
    }
    let (whole, windowed, source) = both(&data);
    assert_eq!(whole.frame_count, None);
    assert_eq!(whole.confidence, Confidence::Derived);
    assert_eq!(whole.duration_ms, windowed.duration_ms);
    // Without a declared count the tail is read too, to say how much was not audio.
    assert!(
        source.read <= 2 * read_head_window(),
        "read {} bytes of {}",
        source.read,
        data.len()
    );
}

fn read_head_window() -> usize {
    audiobook_core::source::HEAD_WINDOW + audiobook_core::source::TAIL_WINDOW
}

#[test]
fn a_bare_mp3_with_no_tag_still_agrees() {
    let header: [u8; 4] = [0xFF, 0xFB, 0x90, 0xC4]; // 128 kbit/s stereo
    let frame_len = 144 * 128_000 / 44_100;
    let mut data = Vec::new();
    for _ in 0..200 {
        data.extend_from_slice(&header);
        data.resize(data.len() + frame_len - 4, 0);
    }
    let (whole, _windowed, _source) = both(&data);
    assert_eq!(whole.container, Some(ContainerKind::Mp3Bare));
}

#[test]
fn an_mp3_whose_tag_is_longer_than_the_head_window_is_read_whole() {
    // Twenty kilobytes of tag: what a file with embedded art looks like. The probe
    // re-reads to the tag's declared size, and every frame in the tag must be found —
    // not only the ones that happened to fit the first window.
    let header: [u8; 4] = [0xFF, 0xFB, 0x50, 0xC0];
    let frame_len = 144 * 64_000 / 44_100;
    let body = 20 * 1024;
    let mut data = b"ID3\x04\x00\x00".to_vec();
    data.push(((body >> 21) & 0x7F) as u8);
    data.push(((body >> 14) & 0x7F) as u8);
    data.push(((body >> 7) & 0x7F) as u8);
    data.push((body & 0x7F) as u8);
    let text = b"\x00A windowed title";
    data.extend_from_slice(b"TIT2");
    data.extend_from_slice(&(u32::try_from(text.len()).unwrap_or(0) + 4).to_be_bytes());
    data.extend_from_slice(&[0u8, 0]);
    data.extend_from_slice(text);
    data.resize(10 + body, 0);
    for _ in 0..100 {
        data.extend_from_slice(&header);
        data.resize(data.len() + frame_len - 4, 0);
    }
    let whole = MediaProbe::probe(&data);
    assert_eq!(whole.title.as_deref(), Some("A windowed title"));
    let (_whole, _windowed, _source) = both(&data);
}

#[test]
fn an_m4b_agrees_whether_read_whole_or_windowed() {
    // Assembled with the container crate's own builders: `ftyp | mdat | moov`, the shape
    // ffmpeg writes, so the `moov` — and the `chpl` inside it — sits at the *end*, past
    // a multi-megabyte `mdat` a head-only window could never reach.
    let data = m4b_with_chpl();
    let (whole, _windowed, source) = both(&data);
    assert_eq!(whole.duration_ms, Some(3_600_000));
    assert_eq!(whole.chapters.len(), 2);
    // Box headers and the moov, plus the head window every probe reads to identify the
    // file. The mdat — ninety-nine percent of this file — is never read, and that is the
    // property worth pinning: the cost does not scale with the file.
    assert!(
        source.read < 2 * audiobook_core::source::HEAD_WINDOW + 4096,
        "read {} of {} bytes",
        source.read,
        data.len()
    );
}

fn m4b_with_chpl() -> Vec<u8> {
    use mp4_core::chapter::{Chapter, ChapterList};
    use mp4_core::container::{container, leaf};
    use mp4_core::moov::{FileType, MovieHeader};

    let chapters = ChapterList {
        chapters: vec![Chapter::new(0, "Opening"), Chapter::new(600_000, "Later")],
    };
    let moov = container(
        *b"moov",
        vec![
            leaf(*b"mvhd", MovieHeader::to_payload(1000, 3_600_000)).expect("infallible"),
            chapters.to_chpl_box(),
        ],
    );
    let mut data = leaf(*b"ftyp", FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(
        &leaf(*b"mdat", vec![0u8; 1_000_000])
            .expect("infallible")
            .to_bytes(),
    );
    data.extend_from_slice(&moov.to_bytes());
    data
}

#[test]
fn the_head_window_alone_identifies_every_container() {
    // `probe_source` detects the container from the head window, so the window must hold
    // whatever detection needs: the "ID3" literal, or an MP4's first box header.
    let mp3 = mp3_with_lame(10, 0, 0);
    let head = read_head(&mut Counting::new(mp3));
    assert_eq!(
        audiobook_core::container::detect(&head),
        Some(ContainerKind::Mp3Id3)
    );

    let m4b = m4b_with_chpl();
    let head = read_head(&mut Counting::new(m4b));
    assert_eq!(
        audiobook_core::container::detect(&head),
        Some(ContainerKind::Mp4)
    );
}

#[test]
fn bytes_that_are_not_audio_report_nothing_rather_than_something() {
    let noise: Vec<u8> = (0..4096u32)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect();
    let whole = MediaProbe::probe(&noise);
    let mut source = Counting::new(noise);
    let windowed = MediaProbe::probe_source(&mut source);
    assert_eq!(whole.container, None);
    assert_eq!(windowed.container, None);
    assert_eq!(windowed.duration_ms, None);
}

#[test]
fn a_file_cut_mid_tag_reports_what_it_has_rather_than_failing() {
    // Truncation is the normal failure of a copying tool, so it is the case the windowed
    // path most has to survive: the tag's declared size points past the end of the file.
    let data = mp3_with_lame(462, 576, 936);
    let cut = &data[..data.len() / 3];
    let whole = MediaProbe::probe(cut);
    let mut source = Counting::new(cut.to_vec());
    let windowed = MediaProbe::probe_source(&mut source);
    assert_agree(&whole, &windowed);
}

#[test]
fn an_m4b_whose_moov_is_first_agrees_too() {
    // `+faststart` shape, which is the mirror of everything else here: the moov sits at
    // the *front*, where a head-only window would find it and a tail-only one would not.
    // The walk must be indifferent to which end the metadata lives at, because muxers
    // choose.
    let data = m4b_with_chpl_at_head();
    let (whole, _windowed, source) = both(&data);
    assert_eq!(whole.duration_ms, Some(3_600_000));
    assert_eq!(whole.chapters.len(), 2);
    assert!(
        source.read < 2 * audiobook_core::source::HEAD_WINDOW + 4096,
        "read {} of {} bytes",
        source.read,
        data.len()
    );
}

fn m4b_with_chpl_at_head() -> Vec<u8> {
    use mp4_core::chapter::{Chapter, ChapterList};
    use mp4_core::container::{container, leaf};
    use mp4_core::moov::{FileType, MovieHeader};

    let chapters = ChapterList {
        chapters: vec![Chapter::new(0, "Opening"), Chapter::new(600_000, "Later")],
    };
    let moov = container(
        *b"moov",
        vec![
            leaf(*b"mvhd", MovieHeader::to_payload(1000, 3_600_000)).expect("infallible"),
            chapters.to_chpl_box(),
        ],
    );
    let mut data = leaf(*b"ftyp", FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(&moov.to_bytes());
    data.extend_from_slice(
        &leaf(*b"mdat", vec![0u8; 1_000_000])
            .expect("infallible")
            .to_bytes(),
    );
    data
}

#[test]
fn an_mp4_with_a_free_box_before_its_mdat_agrees() {
    // Padding between the boxes: a walk that assumed `ftyp` is followed by `mdat` would
    // misread the next header by the free box's width and find nothing at all.
    use mp4_core::chapter::{Chapter, ChapterList};
    use mp4_core::container::{container, leaf};
    use mp4_core::moov::{FileType, MovieHeader};

    let chapters = ChapterList {
        chapters: vec![Chapter::new(0, "Only")],
    };
    let mut data = leaf(*b"ftyp", FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(
        &leaf(*b"free", vec![0u8; 4096])
            .expect("infallible")
            .to_bytes(),
    );
    data.extend_from_slice(
        &leaf(*b"mdat", vec![0u8; 512])
            .expect("infallible")
            .to_bytes(),
    );
    let moov = container(
        *b"moov",
        vec![
            leaf(*b"mvhd", MovieHeader::to_payload(1000, 600_000)).expect("infallible"),
            chapters.to_chpl_box(),
        ],
    );
    data.extend_from_slice(&moov.to_bytes());

    let (whole, _windowed, _source) = both(&data);
    assert_eq!(whole.duration_ms, Some(600_000));
    assert_eq!(whole.chapters.len(), 1);
}

/// A 48 kHz MP3, because its frame length is a whole number of bytes at every bitrate
/// and no padding bit is ever set — which makes the frame arithmetic exact, so a test can
/// state the duration it expects in closed form rather than walking the file to find out.
// 48 kHz is sample-rate index 01: byte two is bitrate 0101 with the rate bits 01 after
// it. The first attempt at this fixture used 0x50, whose rate bits are 00 - 44.1 kHz -
// and its own arithmetic then disagreed with the reader, because a 44.1 kHz frame is 208
// bytes where the fixture stepped 192. The fixture was wrong, and the disagreement was
// the only reason anyone found out.
const EXACT_HEADER: [u8; 4] = [0xFF, 0xFB, 0x54, 0xC4]; // 64 kbit/s mono 48 kHz
const EXACT_FRAME_LEN: usize = 192;
const EXACT_SAMPLES_PER_FRAME: u64 = 1152;
const EXACT_RATE: u64 = 48_000;

/// `frames` frames of audio plus a trailing tag, and the duration the audio alone is.
fn mp3_with_trailing_tag(tag: &[u8]) -> (Vec<u8>, u64) {
    let frames = 400u64;
    let mut data = Vec::new();
    for _ in 0..frames {
        data.extend_from_slice(&EXACT_HEADER);
        data.resize(data.len() + EXACT_FRAME_LEN - 4, 0);
    }
    let audio_len = data.len();
    data.extend_from_slice(tag);
    // frames * samples / rate, in whole milliseconds, with no rounding anywhere: 400
    // frames of 1152 samples at 48 kHz is 9.6 s exactly.
    let expected_ms = frames * EXACT_SAMPLES_PER_FRAME * 1_000 / EXACT_RATE;
    let _ = audio_len;
    (data, expected_ms)
}

#[test]
fn an_mp3_with_only_a_trailing_id3v1_tag_agrees() {
    // No ID3v2 at all: the tag lives at the end, where only the tail window looks, and
    // the audio's byte count must lose those 128 bytes or the CBR duration is long by
    // however long the tag is.
    // An ID3v1 tag *starts* with "TAG" and is 128 bytes in total; writing the magic last
    // builds no tag a tagger would recognise, and the reader correctly finds none.
    let mut tag = b"TAG".to_vec();
    tag.extend_from_slice(&[0u8; 125]);
    let (data, expected_ms) = mp3_with_trailing_tag(&tag);
    let whole = MediaProbe::probe(&data);
    assert_eq!(
        whole.duration_ms,
        Some(expected_ms),
        "128 bytes of tag are not 128 bytes of audio"
    );
    let (_whole, _windowed, _source) = both(&data);
}

#[test]
fn an_mp3_with_a_trailing_ape_tag_agrees() {
    // APE tags are what MusicBrainz taggers write, and they are bigger than ID3v1 by far
    // — long enough that leaving one in the audio byte count is visible in minutes.
    // Sixty-four bytes of items and a 32-byte footer whose size field says 96, which is
    // the layout mutagen writes and reads: magic first, size at offset twelve.
    let mut tag = vec![0u8; 64];
    tag.extend_from_slice(b"APETAGEX");
    tag.extend_from_slice(&2000u32.to_le_bytes());
    tag.extend_from_slice(&96u32.to_le_bytes());
    tag.extend_from_slice(&0u32.to_le_bytes());
    tag.extend_from_slice(&0u32.to_le_bytes());
    tag.extend_from_slice(&[0u8; 8]);
    let (data, expected_ms) = mp3_with_trailing_tag(&tag);
    let whole = MediaProbe::probe(&data);
    assert_eq!(whole.duration_ms, Some(expected_ms));
    let (_whole, _windowed, _source) = both(&data);
}

#[test]
fn debug_tail() {
    let mut tag = vec![0u8; 125];
    tag.extend_from_slice(b"TAG");
    let (data, _e) = mp3_with_trailing_tag(&tag);
    let n = data.len();
    let tail = &data[n - 8192..];
    println!(
        "tail len={} last16={:?}",
        tail.len(),
        &tail[tail.len() - 16..]
    );
    println!(
        "trailing(8192)={}",
        audiobook_core::source::trailing_tag_bytes(tail)
    );
}
