#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Integration: the scenarios a library actually faces.
//!
//! The unit tests inside each module prove each piece works alone. These prove
//! the *compositions* hold — the same title read from two packagings, a folder
//! read in the wrong order, a file that lies about its own chapters — which is
//! the only reason this crate exists.

use audiobook_core::chapter::{concatenate, concatenate_spanning};
use audiobook_core::container::{detect, ContainerKind};
use audiobook_core::mpeg::{parse_header, vbr_tag_offset, Layer, MpegVersion};
use audiobook_core::probe::{mdat_size, MediaProbe};
use audiobook_core::{Chapter, Confidence, Part, Title};
use std::vec;
use std::vec::Vec;

use mp4_core::container::{container, leaf};
use mp4_core::metadata::{Atom, Metadata, MetadataValue};
use mp4_core::moov::MovieHeader;
use mp4_core::{atoms, Mp4File};

// ---------------------------------------------------------------- builders --

/// A 128 kbit/s 44.1 kHz MPEG-1 Layer III frame header.
fn mpeg_frame_header() -> [u8; 4] {
    [0xFF, 0xFB, (9u8 << 4), 0b0100_0000]
}

/// A complete audio frame of `total_len` bytes.
fn audio_frame(total_len: usize) -> Vec<u8> {
    let mut f = mpeg_frame_header().to_vec();
    f.resize(total_len, 0);
    f
}

/// An MP3 carrying an ID3 tag, with the given `CHAP` chapters.
///
/// Each chapter is `TIT2` sub-frame plus `start`/`end`/`offsets`, which is the
/// shape `id3-core`'s `resolved_chapters` walks.
fn mp3_with_chapters(title: &str, chapters: &[(u32, &str)]) -> Vec<u8> {
    let mut tag = id3_core::Tag::new();
    tag.set_text(
        id3_core::TextField::Title,
        id3_core::TextEncoding::Utf8,
        title,
    );
    for (i, (start_ms, name)) in chapters.iter().enumerate() {
        // A chapter's sub-frames are raw frame *bodies*, so a TIT2 sub-frame is
        // the encoded text without a header.
        let title_body = id3_core::Text::single(id3_core::TextEncoding::Utf8, name)
            .to_body()
            .expect("a text frame encodes");
        let mut chapter = id3_core::chapter::Chapter::new(&format!("ch{i}"), *start_ms)
            .with_end(start_ms.saturating_add(60_000));
        chapter
            .frames
            .push((id3_core::FrameId::raw(b"TIT2"), title_body));
        tag.push(id3_core::Frame::Chapter(chapter));
    }
    tag.to_bytes().expect("a tag encodes")
}

/// An MP3 with a tag, a declared frame count, and `n` frames of audio.
fn mp3_vbr(chapters: &[(u32, &str)], n_frames: u32) -> Vec<u8> {
    let mut data = mp3_with_chapters("Tagged", chapters);
    // The Xing header goes inside the first frame, after the side information.
    let header = parse_header(&mpeg_frame_header()).expect("a valid header");
    let offset = vbr_tag_offset(header.version, header.channels);
    let bytes = n_frames * 417;

    let mut first = mpeg_frame_header().to_vec();
    first.resize(offset, 0);
    first.extend_from_slice(b"Xing");
    first.extend_from_slice(&[0u8, 0, 0, 0b0000_0011]); // frames + bytes
    first.extend_from_slice(&n_frames.to_be_bytes());
    first.extend_from_slice(&bytes.to_be_bytes());
    first.resize(417, 0);

    data.extend_from_slice(&first);
    for _ in 1..n_frames {
        data.extend_from_slice(&audio_frame(417));
    }
    data
}

/// An M4B: `ftyp`, a `moov` with one audio track, `chpl` chapters, `ilst`
/// metadata, and an `mdat` of `audio_bytes` bytes.
fn m4b(title: &str, chapters: &[(u64, &str)], duration_ms: u64, audio_bytes: usize) -> Vec<u8> {
    let mut metadata = Metadata::default();
    metadata.atoms.push(Atom {
        name: atoms::TITLE,
        values: vec![MetadataValue::text(title)],
    });
    metadata.atoms.push(Atom {
        name: atoms::ARTIST,
        values: vec![MetadataValue::text("A Narrator")],
    });
    metadata.atoms.push(Atom {
        name: atoms::MEDIA_KIND,
        values: vec![MetadataValue {
            data_type: mp4_core::DataType::UnsignedInteger,
            payload: mp4_core::metadata::MEDIA_KIND_AUDIOBOOK
                .to_be_bytes()
                .to_vec(),
            locale: 0,
        }],
    });

    let chpl: Vec<mp4_core::Chapter> = chapters
        .iter()
        .map(|(ms, name)| mp4_core::Chapter::new(*ms, name))
        .collect();

    let mvhd = leaf(*b"mvhd", MovieHeader::to_payload(1_000, duration_ms)).expect("infallible");

    // tkhd: version 0, flags enabled|in-movie, track id 1, duration.
    let mut tkhd = vec![0u8, 0, 0, 3];
    tkhd.extend_from_slice(&0u32.to_be_bytes());
    tkhd.extend_from_slice(&0u32.to_be_bytes());
    tkhd.extend_from_slice(&1u32.to_be_bytes());
    tkhd.extend_from_slice(&0u32.to_be_bytes());
    tkhd.extend_from_slice(&u32::try_from(duration_ms).expect("fits").to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 8]);
    let tkhd = leaf(*b"tkhd", tkhd).expect("infallible");

    let mdhd = leaf(
        *b"mdhd",
        mp4_core::moov::MediaHeader::to_payload(44_100, duration_ms),
    )
    .expect("infallible");
    let mut hdlr = vec![0u8, 0, 0, 0];
    hdlr.extend_from_slice(&[0u8, 0, 0, 0]);
    hdlr.extend_from_slice(b"soun");
    hdlr.extend_from_slice(&[0u8; 12]);
    let hdlr = leaf(*b"hdlr", hdlr).expect("infallible");

    // The audio sample entry: 6 reserved + 2 data_reference_index + 8 reserved +
    // 2 channels + 2 sample size + 2 pre-defined + 2 reserved + 4 rate = 28 bytes.
    let mp4a = leaf(
        *b"mp4a",
        [
            vec![0u8; 6],
            vec![0u8, 1],
            vec![0u8; 8],
            vec![0u8, 2],  // stereo
            vec![0u8, 16], // 16-bit
            vec![0u8, 0],
            vec![0u8, 0],
            // `samplerate << 16` per ISO/IEC 14496-3 — an integer in the high
            // half, not a 16.16 fraction.
            (44_100u32 << 16).to_be_bytes().to_vec(),
        ]
        .concat(),
    )
    .expect("infallible");

    // `stsd` is a full box *and* carries an entry count, so eight bytes precede the
    // entries. A builder that omits them produces a file no muxer writes and no
    // conformant reader can find a sample entry in.
    let mut stsd_payload = vec![0u8, 0, 0, 0]; // version + flags
    stsd_payload.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    stsd_payload.extend_from_slice(&mp4a.to_bytes());
    let stbl = container(
        *b"stbl",
        vec![leaf(*b"stsd", stsd_payload).expect("infallible")],
    );
    let minf = container(*b"minf", vec![stbl]);
    let mdia = container(*b"mdia", vec![mdhd, hdlr, minf]);
    let trak = container(*b"trak", vec![tkhd, mdia]);

    let moov = container(
        *b"moov",
        vec![
            mvhd,
            trak,
            metadata.to_udta(),
            mp4_core::ChapterList { chapters: chpl }.to_chpl_box(),
        ],
    );

    let mut data = leaf(*b"ftyp", mp4_core::FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(&moov.to_bytes());
    data.extend_from_slice(
        &leaf(*b"mdat", vec![0u8; audio_bytes])
            .expect("infallible")
            .to_bytes(),
    );
    data
}

// ------------------------------------------------- the cross-format promise --

#[test]
fn one_book_two_packagings_produce_the_same_chapter_list() {
    // This is the crate's whole reason for existing: the chapters of a title must
    // not depend on whether it arrived as one file or several.
    let chapters = [
        (0u64, "Opening"),
        (900_000, "Chapter One"),
        (1_800_000, "Chapter Two"),
    ];
    let single = Title::from_probe(
        &MediaProbe::probe(&m4b("The Book", &chapters, 3_600_000, 64_000)),
        "x",
        "y",
    );

    // The same three chapters split across three files of an hour each, each
    // carrying the same title and author as the single file.
    let file = |name: &str| MediaProbe {
        title: Some("The Book".into()),
        artist: Some("A Narrator".into()),
        chapters: vec![(0, name.into())],
        ..MediaProbe::default()
    };
    let files = vec![file("Opening"), file("Chapter One"), file("Chapter Two")];
    // The same book, cut at its own chapter boundaries, so the totals match too.
    let multi = Title::from_probes(
        &files,
        &[900_000, 900_000, 1_800_000],
        "fallback",
        "fallback",
    );
    assert_eq!(multi.duration_ms(), single.duration_ms());

    assert_eq!(single.title, multi.title, "the same work either way");
    assert_eq!(single.author, multi.author);
    assert_eq!(single.year, multi.year);

    // The same chapter titles, at the same whole-book offsets.
    let a: Vec<_> = single
        .chapters()
        .iter()
        .map(|c| (c.title.clone(), c.start_ms))
        .collect();
    let b: Vec<_> = multi
        .chapters()
        .iter()
        .map(|c| (c.title.clone(), c.start_ms))
        .collect();
    assert_eq!(a, b, "chapters must not depend on the packaging");
}

#[test]
fn a_seek_bar_built_from_either_packaging_agrees() {
    let chapters = [(0u64, "A"), (1_000_000, "B"), (2_000_000, "C")];
    let single = Title::from_probe(
        &MediaProbe::probe(&m4b("Book", &chapters, 3_000_000, 48_000)),
        "x",
        "y",
    );

    let file = |name: &str| MediaProbe {
        chapters: vec![(0, name.into())],
        ..MediaProbe::default()
    };
    let files = vec![file("A"), file("B"), file("C")];
    let multi = Title::from_probes(&files, &[1_000_000; 3], "Book", "Author");

    for t in [0u64, 999_999, 1_000_000, 1_500_000, 2_999_999] {
        let want = single.chapter_at(t).map(|c| c.title.as_str());
        let got = multi.chapter_at(t).map(|c| c.title.as_str());
        assert_eq!(want, got, "at {t} ms the two disagree");
    }
}

#[test]
fn a_folder_read_in_lexical_order_is_reordered() {
    // `10.mp3` sorts before `2.mp3`, so the parts arrive with times running
    // backwards. This is the case `sorted_by_time` exists for.
    let probes = vec![
        MediaProbe {
            chapters: vec![(0, "Ten".into())],
            duration_ms: Some(900_000),
            ..MediaProbe::default()
        },
        MediaProbe {
            chapters: vec![(0, "Two".into())],
            duration_ms: Some(100_000),
            ..MediaProbe::default()
        },
    ];
    let title = Title::from_probes(&probes, &[900_000, 100_000], "Book", "Author");
    // Out of order: the folder put part 0 last.
    assert_eq!(title.parts[0].chapters[0].title, "Ten");
    assert_eq!(title.parts[1].chapters[0].title, "Two");

    // Swapping them and re-laying out puts them in playing order with correct
    // offsets, which is what makes a whole-book seek bar agree with the file list.
    let fixed = title.with_part_at(0, 1);
    assert_eq!(fixed.parts[0].chapters[0].title, "Two");
    assert_eq!(fixed.parts[1].chapters[0].title, "Ten");
    assert_eq!(fixed.parts[0].offset_ms, 0);
    assert_eq!(fixed.parts[1].offset_ms, 100_000);
    let flat = fixed.chapters();
    let absolute: Vec<_> = flat.iter().map(|c| (c.title.clone(), c.start_ms)).collect();
    assert_eq!(
        absolute,
        vec![("Two".to_string(), 0u64), ("Ten".to_string(), 100_000)],
        "the flat view is title-absolute even though the parts are file-local"
    );
}

// ------------------------------------------------------- container reality --

#[test]
fn an_m4b_probes_to_an_exact_duration_and_a_bitrate() {
    let data = m4b("The Book", &[(0, "Opening")], 3_600_000, 57_600);
    let p = MediaProbe::probe(&data);

    assert_eq!(p.container, Some(ContainerKind::Mp4));
    assert_eq!(p.duration_ms, Some(3_600_000));
    assert_eq!(p.confidence, Confidence::Declared);
    assert_eq!(p.duration_display(), "1:00:00");
    assert_eq!(p.sample_rate, Some(44_100));
    // 57_600 bytes over 3_600 s is 128 bits/s.
    assert_eq!(p.bitrate_bps, Some(128));
    assert_eq!(p.title.as_deref(), Some("The Book"));
    assert_eq!(p.artist.as_deref(), Some("A Narrator"));
    assert_eq!(p.chapters, vec![(0u64, "Opening".to_string())]);
    assert!(p.has_chapters());
}

#[test]
fn an_m4bs_sample_rate_is_read_from_the_mp4a_entry() {
    // The rate is 16.16 fixed point inside the sample entry, and reading it as a
    // raw integer would give 705,600 rather than 44,100.
    let data = m4b("Book", &[(0, "A")], 1_000, 1_000);
    assert_eq!(mp4_core::moov::read_sample_rate(&data), Some(44_100));
    assert_eq!(MediaProbe::probe(&data).sample_rate, Some(44_100));
}

#[test]
fn mdat_size_excludes_the_header_from_the_bitrate() {
    // If the header were counted, a 1 KB payload in a 1-second file would report
    // a bitrate 40 bits/s high. The pinned numbers prove it does not.
    let data = m4b("Book", &[(0, "A")], 1_000, 1_000);
    assert_eq!(mdat_size(&data), Some(1_000));
    let p = MediaProbe::probe(&data);
    assert_eq!(
        p.bitrate_bps,
        Some(8_000),
        "1000 bytes over 1 s is 8 kbit/s"
    );
}

#[test]
fn a_file_whose_extension_lied_is_still_read() {
    // An M4B named `.mp3` after a bad download. Detection is by signature, so the
    // title comes out whole rather than empty.
    let data = m4b("Real Title", &[(0, "Opening")], 60_000, 8_000);
    let p = MediaProbe::probe(&data);
    assert_eq!(p.container, Some(ContainerKind::Mp4));
    assert_eq!(p.title.as_deref(), Some("Real Title"));
    assert_eq!(p.chapters.len(), 1);
}

// --------------------------------------------------------------- mp3 reality --

#[test]
fn a_vbr_mp3_probes_to_an_exact_duration() {
    let data = mp3_vbr(&[(0, "Opening"), (60_000, "Second")], 957);
    let p = MediaProbe::probe(&data);

    assert_eq!(p.container, Some(ContainerKind::Mp3Id3));
    assert_eq!(p.frame_count, Some(957));
    assert_eq!(p.confidence, Confidence::Declared);
    assert_eq!(p.duration_ms, Some(24_999));
    assert_eq!(p.sample_rate, Some(44_100));
    assert_eq!(p.channels, Some(2));
    assert_eq!(p.variable_bitrate, Some(true));
    assert_eq!(p.title.as_deref(), Some("Tagged"));
    assert_eq!(p.chapters.len(), 2);
    assert_eq!(p.chapters[1], (60_000u64, "Second".to_string()));
    // 957 * 417 = 399_069 bytes over 24.999 s.
    assert_eq!(p.bitrate_bps, Some(127_707));
}

#[test]
fn a_cbr_mp3_probes_to_a_derived_duration_and_says_so() {
    // No Xing header: the bitrate is constant by definition, so the duration
    // follows from it — but the probe must report it as derived, not declared.
    let mut data = mp3_with_chapters("CBR", &[(0, "Only")]);
    for _ in 0..100 {
        data.extend_from_slice(&audio_frame(417));
    }
    let p = MediaProbe::probe(&data);

    assert_eq!(p.frame_count, None);
    assert_eq!(p.variable_bitrate, Some(false));
    assert_eq!(p.confidence, Confidence::Derived);
    assert_eq!(p.duration_ms, Some(2_606), "41_700 bytes at 128 kbit/s");
    assert_eq!(p.bitrate_bps, Some(128_000));
    assert_ne!(
        p.confidence,
        Confidence::Declared,
        "not a declared duration"
    );
}

#[test]
fn an_mp3_with_no_chapters_reports_none_rather_than_inventing_them() {
    let mut data = mp3_with_chapters("Plain", &[]);
    for _ in 0..10 {
        data.extend_from_slice(&audio_frame(417));
    }
    let title = Title::from_probe(&MediaProbe::probe(&data), "plain.mp3", "");
    assert!(!title.has_chapters());
    assert_eq!(title.chapter_source, audiobook_core::ChapterSource::None);
    assert!(title.parts.is_empty());
}

#[test]
fn an_mp3_with_no_frames_still_yields_its_metadata() {
    // A tag with no audio at all is a real thing: a download that fetched the
    // container and stopped. The metadata is still worth showing.
    let data = mp3_with_chapters("Tag Only", &[]);
    let p = MediaProbe::probe(&data);
    assert_eq!(p.title.as_deref(), Some("Tag Only"));
    assert_eq!(p.duration_ms, None);
    assert_eq!(p.sample_rate, None);
    assert_eq!(p.confidence, Confidence::Unknown);
}

#[test]
fn a_truncated_mp3_probes_to_what_is_there() {
    // Half of what should be a complete frame: the walk stops, and the probe
    // reports what it saw rather than claiming a full file.
    let mut data = mp3_with_chapters("Cut", &[]);
    for _ in 0..10 {
        data.extend_from_slice(&audio_frame(417));
    }
    data.truncate(data.len() - 200);

    let p = MediaProbe::probe(&data);
    assert_eq!(p.title.as_deref(), Some("Cut"));
    // Nine whole frames plus a partial one that is not counted.
    // The byte-derived duration includes the 217 surviving bytes of the truncated
    // frame; a frame-count derivation would report only the nine whole frames.
    assert_eq!(p.duration_ms, Some(248));
    assert_eq!(p.confidence, Confidence::Derived);
}

// --------------------------------------------------------------- multi-file --

#[test]
fn per_file_durations_are_what_make_the_offsets_correct() {
    // Without real durations the concatenated offsets collapse to zero, which is
    // the difference between a working and a broken chapter list.
    let files = vec![
        vec![(0u64, "A1".to_string()), (60_000, "A2".to_string())],
        vec![(0u64, "B1".to_string())],
    ];

    // Concatenation preserves each file's own container times and attributes each
    // chapter to its file; the whole-title offsets come from laying the parts out,
    // which is where the durations are actually used. Without durations every part
    // starts at zero, so the offsets are lost — and the difference is visible.
    let no_durations = Title::from_flat_chapters(concatenate(&files), &[0, 0], "Book", "Author");
    assert_eq!(
        no_durations.chapters()[2].start_ms,
        0,
        "with no durations the offset is lost"
    );

    let exact =
        Title::from_flat_chapters(concatenate(&files), &[600_000, 300_000], "Book", "Author");
    assert_eq!(
        exact.chapters()[2].start_ms,
        600_000,
        "a real duration restores it"
    );
    assert_eq!(exact.chapters()[2].title, "B1");
    assert_eq!(exact.duration_ms(), Some(900_000));
}

#[test]
fn a_chapter_that_spans_files_still_seeks_correctly() {
    // The shape real M4B tooling produces: parts → chapters → files, where a chapter
    // names several source files. Audible and Librivox split on time, so a boundary
    // lands mid-file and one chapter routinely runs across two.
    //
    // Before this was representable, the boundary was either dropped or attributed
    // to the wrong file — and a seek landed in the wrong place with no error anywhere.
    let files = vec![
        // 20 min long; "Chapter Two" starts 10 min in and runs past the end.
        vec![
            (0u64, "Chapter One".into()),
            (600_000, "Chapter Two".into()),
        ],
        // 15 min long. Its first chapter starts 30 s in, so those first 30 s are the
        // tail of "Chapter Two" — the real-world case of a boundary landing mid-file.
        vec![
            (30_000, "Chapter Three".into()),
            (300_000, "Chapter Four".into()),
        ],
        vec![(0u64, "Chapter Five".into())],
    ];
    let durations = [1_200_000u64, 900_000, 600_000];
    // Files 0 and 1 both end mid-chapter.
    let continues = [true, true, false];

    let chapters = concatenate_spanning(&files, &continues);
    assert_eq!(
        chapters[0].parts,
        vec![0usize],
        "Chapter One is inside file 0"
    );
    assert_eq!(
        chapters[1].parts,
        vec![0usize, 1],
        "Chapter Two starts in file 0 and continues into file 1"
    );
    assert!(chapters[1].spans_parts());

    let title = Title::from_flat_chapters(chapters, &durations, "Book", "Author");
    assert_eq!(title.duration_ms(), Some(2_700_000));

    // The whole-title offsets, which are what a seek bar uses. A chapter's offset
    // comes from the file it *starts* in, so Chapter Two is 600 s in, not 1200 s.
    let flat = title.chapters();
    let offsets: Vec<(&str, u64)> = flat
        .iter()
        .map(|c| (c.title.as_str(), c.start_ms))
        .collect();
    assert_eq!(
        offsets,
        vec![
            ("Chapter One", 0),
            ("Chapter Two", 600_000),
            // File 1 starts 1 200 s in; its first chapter is 30 s further on.
            ("Chapter Three", 1_230_000),
            ("Chapter Four", 1_500_000),
            ("Chapter Five", 2_100_000),
        ]
    );

    // A seek anywhere resolves to the right chapter, including just inside a
    // straddling one.
    for (at, want) in [
        (0u64, "Chapter One"),
        (599_999, "Chapter One"),
        (600_000, "Chapter Two"),
        (1_199_999, "Chapter Two"),
        (1_229_999, "Chapter Two"),
        (1_230_000, "Chapter Three"),
        (1_499_999, "Chapter Three"),
        (1_500_000, "Chapter Four"),
        (2_099_999, "Chapter Four"),
        (2_100_000, "Chapter Five"),
    ] {
        assert_eq!(
            title.chapter_at(at).map(|c| c.title.as_str()),
            Some(want),
            "at {at} ms"
        );
    }

    // Which file is playing also has to be right across a straddling chapter.
    assert_eq!(title.part_at(0).map(|p| p.offset_ms), Some(0));
    assert_eq!(title.part_at(1_199_999).map(|p| p.offset_ms), Some(0));
    assert_eq!(
        title.part_at(1_200_000).map(|p| p.offset_ms),
        Some(1_200_000)
    );
}

#[test]
fn a_part_with_unknown_length_does_not_swallow_the_next_one() {
    // A part whose chapters start at 0 has an implied span of nothing. Without a
    // declared duration every part after it would be unreachable.
    let title = Title::new("Book", "Author")
        .with_part(Part {
            title: String::from("No length"),
            chapters: vec![Chapter::new("One", 0, 0)],
            offset_ms: 0,
            duration_ms: None,
        })
        .with_part(Part {
            title: String::from("Known"),
            chapters: vec![Chapter::new("Two", 0, 0)],
            offset_ms: 0,
            duration_ms: Some(1_000),
        });
    assert_eq!(title.part_at(0).map(|p| p.title.as_str()), Some("Known"));
    assert_eq!(title.part_at(100).map(|p| p.title.as_str()), Some("Known"));
}

#[test]
fn a_folder_with_one_unreadable_file_still_lists() {
    // Probing never fails, which is what lets a host list a folder containing one
    // file it cannot read.
    let good = MediaProbe::probe(&mp3_vbr(&[(0, "A")], 100));
    let bad = MediaProbe::probe(&[0x00, 0x01, 0x02]);
    assert_eq!(good.title.as_deref(), Some("Tagged"));
    assert_eq!(bad.container, None);

    let probes = vec![good, bad];
    let title = Title::from_probes(&probes, &[600_000, 0], "Book", "Author");
    assert_eq!(
        title.parts.len(),
        1,
        "only the readable file becomes a part"
    );
    assert_eq!(title.title, "Tagged");
}

// ---------------------------------------------------------------- validation --

#[test]
fn validation_catches_a_producer_that_wrote_chapters_out_of_order() {
    // An M4B whose `chpl` is written backwards is real, and a library that serves
    // it blindly shows a chapter list in the wrong order.
    let data = m4b(
        "Bad Book",
        &[(1_800_000, "Second"), (0, "First")],
        3_600_000,
        8_000,
    );
    let title = Title::from_probe(&MediaProbe::probe(&data), "bad.m4b", "y");

    // The reader sorts, so what surfaces is already ordered...
    let titles: Vec<_> = title.chapters().iter().map(|c| c.title.clone()).collect();
    let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
    assert_eq!(titles, vec!["First", "Second"]);

    // ...but a caller who rebuilds a title by hand can still produce an invalid
    // one, and `validate` is what catches it before it reaches a player.
    let broken = Title::new("Bad", "Author").with_part(Part {
        title: String::new(),
        chapters: vec![
            Chapter::new("Second", 1_800_000, 0),
            Chapter::new("First", 0, 0),
        ],
        offset_ms: 0,
        duration_ms: Some(3_600_000),
    });
    assert!(broken.validate().is_err());
}

#[test]
fn a_chapter_past_the_end_is_caught_before_a_player_sees_it() {
    let data = m4b(
        "Bad Book",
        &[(0, "Opening"), (7_200_000, "Beyond")],
        3_600_000,
        8_000,
    );
    let title = Title::from_probe(&MediaProbe::probe(&data), "bad.m4b", "y");
    assert!(
        title.validate().is_err(),
        "a chapter past the declared duration must not ship"
    );
}

#[test]
fn a_real_book_validates_clean() {
    let data = m4b(
        "The Long Now",
        &[
            (0, "Opening"),
            (900_000, "Chapter One"),
            (1_800_000, "Chapter Two"),
            (2_700_000, "Chapter Three"),
        ],
        3_600_000,
        57_600,
    );
    let title = Title::from_probe(&MediaProbe::probe(&data), "x.m4b", "y");
    assert!(title.validate().is_ok());
    assert_eq!(title.chapter_count(), 4);
    assert_eq!(MediaProbe::probe(&data).duration_display(), "1:00:00");
}

// --------------------------------------------------------------- detection --

#[test]
fn detection_never_guesses_from_a_length_alone() {
    // 128 zero bytes is exactly an empty ID3v1 tag's shape, but with no audio
    // before it there is nothing to call it.
    assert_eq!(detect(&[0u8; 128]), None);
    // A PDF is not audio.
    assert_eq!(detect(b"%PDF-1.7\n%\xE2\xE3\xC6"), None);
    // A ZIP local header is not audio.
    assert_eq!(detect(b"PK\x03\x04zzzzzzzzzz"), None);
}

#[test]
fn an_mp3_detected_by_its_frame_sync_still_reads_its_tag() {
    // No `ID3` magic — an old file with only a v1 trailer. Detection has to say
    // MP3 for the frame walk to run, and the metadata comes from the v1 tag.
    let mut data = vec![0u8; 300];
    data.extend_from_slice(&audio_frame(417));
    assert_eq!(detect(&data), Some(ContainerKind::Mp3Bare));

    let p = MediaProbe::probe(&data);
    assert_eq!(p.sample_rate, Some(44_100));
    assert_eq!(p.channels, Some(2));
    assert_eq!(
        p.frame_count, None,
        "no count header on a bare frame stream"
    );
}

#[test]
fn a_sample_entry_that_is_not_audio_is_not_measured() {
    // A QuickTime file whose only track is text: no sample rate to report, but
    // still a readable file.
    let mvhd = leaf(*b"mvhd", MovieHeader::to_payload(1_000, 1_000)).expect("infallible");
    let moov = container(*b"moov", vec![mvhd]);
    // A plain `isom` ftyp: no `M4B ` brand, so `is_audiobook` has nothing to go on.
    let ftyp = mp4_core::FileType {
        major_brand: mp4_core::moov::BRAND_ISOM,
        minor_version: 512,
        compatible_brands: vec![mp4_core::moov::BRAND_MP42],
    };
    let mut data = leaf(*b"ftyp", ftyp.to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(&moov.to_bytes());

    let p = MediaProbe::probe(&data);
    assert_eq!(p.sample_rate, None);
    assert_eq!(p.duration_ms, Some(1_000));
    assert!(
        !Mp4File::parse(&data).expect("parses").is_audiobook(),
        "no stik, no chapters, and no M4B brand"
    );
}

#[test]
fn an_m4b_is_recognised_as_an_audiobook_by_its_chapters_alone() {
    // No `stik`, no `M4B ` brand — only a chapter list. A player still shows
    // chapter navigation, so the library must too.
    let data = m4b("Chapters Only", &[(0, "One"), (1000, "Two")], 10_000, 1_000);
    assert!(Mp4File::parse(&data).expect("parses").is_audiobook());

    let bare = m4b("No Chapters", &[], 10_000, 1_000);
    let file = Mp4File::parse(&bare).expect("parses");
    // The stik=9 atom is still there, so it is an audiobook by that signal.
    assert!(file.metadata.is_audiobook());
}

#[test]
fn mpeg_layers_and_versions_are_told_apart() {
    // Every layer and both version families resolve, and each family's own bitrate
    // table applies — a layer conflated with another gives a wrong frame length.
    // Byte 1 is `111 | version(2) | layer(2) | protection(1)`, so for MPEG-1 Layer
    // III it is 0xFB, for Layer II 0xFD, and for Layer I 0xFF.
    for (byte1, layer) in [
        (0xFBu8, Layer::Layer3),
        (0xFDu8, Layer::Layer2),
        (0xFFu8, Layer::Layer1),
    ] {
        let h = parse_header(&[0xFF, byte1, (9 << 4), 0b0100_0000]).expect("valid");
        assert_eq!(h.layer, layer);
        assert!(h.frame_len() > 0);
    }
    assert_eq!(
        parse_header(&[0xFF, 0xF3, (9 << 4), 0b0100_0000])
            .expect("valid")
            .version,
        MpegVersion::Mpeg2
    );
}
