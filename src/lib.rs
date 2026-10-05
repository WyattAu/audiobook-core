//! Audiobook domain model — the layer that turns containers into a library.
//!
//! `id3-core` reads tags from MP3, `mp4-core` reads boxes from MP4, and
//! `dsp-core` measures the audio. None of them knows what a *book* is. This
//! crate is where the three meet: one title, however it was packaged, with its
//! chapters, its parts, and its audio in a shape a library can serve.
//!
//! # The problem this solves
//!
//! An audiobook arrives as any of:
//!
//! | On disk | Format | Chapters | Metadata |
//! |---|---|---|---|
//! | one `.mp3` | ID3v2 | `CHAP`/`CTOC` frames | ID3 text frames |
//! | many `.mp3` in a folder | ID3v2 each | per-file `CHAP` | per-file frames |
//! | one `.m4b` | ISO-BMFF | `chpl` or a text track | iTunes `ilst` |
//! | one `.m4a` | ISO-BMFF | usually none | iTunes `ilst` |
//!
//! A library that special-cases per format ends up with four code paths that
//! disagree about what a chapter is. This crate has one.
//!
//! # What is *not* here
//!
//! - **No file I/O, no decoding, no storage.** A [`Title`] is built from bytes
//!   the caller supplies; where those bytes came from is the host's business.
//! - **No guessing.** A title with no chapters has no chapters — this crate
//!   never invents a chapter list from file boundaries, because a "chapter" per
//!   track is a fabrication that shows up in a player's UI as a lie.
//!
//! # Example
//!
//! One title, assembled from either packaging. Neither branch invents a chapter
//! that the file did not declare:
//!
//! ```
//! use audiobook_core::{Chapter, MediaProbe, Part, Title};
//!
//! // A single M4B, as `MediaProbe` would hand it over.
//! let probe = MediaProbe {
//!     duration_ms: Some(3_600_000),
//!     confidence: audiobook_core::Confidence::Declared,
//!     title: Some("The Long Now".into()),
//!     chapters: vec![(0, "Opening".into()), (900_000, "Chapter One".into())],
//!     ..MediaProbe::default()
//! };
//!
//! let title = Title::from_probe(&probe, "book.m4b", "unknown");
//! assert_eq!(title.title, "The Long Now");
//! assert_eq!(title.chapter_count(), 2);
//! assert_eq!(
//!     title.chapter_at(1_000_000).map(|c| c.title.as_str()),
//!     Some("Chapter One")
//! );
//! assert!(title.validate().is_ok());
//!
//! // The same book as three files. The chapters carry real per-file lengths, so
//! // the offsets are right rather than merely plausible.
//! let file = |name: &str| MediaProbe {
//!     chapters: vec![(0, name.into())],
//!     ..MediaProbe::default()
//! };
//! let files = vec![file("One"), file("Two"), file("Three")];
//! let title = Title::from_probes(&files, &[600_000, 600_000, 600_000], "Book", "Author");
//! assert_eq!(title.parts.len(), 3);
//! assert_eq!(title.duration_ms(), Some(1_800_000));
//!
//! // Assembly fills in each part's offset from the lengths, so `chapters()` — which
//! // is title-absolute — agrees with the parts, whose chapters stay file-local.
//! assert_eq!(title.chapters()[1].start_ms, 600_000);
//! assert_eq!(title.parts[1].offset_ms, 600_000);
//! assert_eq!(title.parts[1].chapters[0].start_ms, 0);
//!
//! // A title with nothing declared has no chapters, and says so.
//! let bare = Title::from_probe(&MediaProbe::default(), "mystery.mp3", "");
//! assert!(!bare.has_chapters());
//! assert!(!bare.has_exact_duration());
//!
//! // A hand-built title, when the caller already knows the shape.
//! let manual = Title::new("Book", "Author").with_part(Part {
//!     title: String::new(),
//!     chapters: vec![Chapter::new("Only", 0, 0)],
//!     offset_ms: 0,
//!     duration_ms: Some(1_000),
//! });
//! assert_eq!(manual.chapter_count(), 1);
//! ```

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod chapter;
pub mod container;
pub mod error;
pub mod mpeg;
pub mod probe;
pub mod title;

pub use chapter::{Chapter, ChapterSource, Part};
pub use container::ContainerKind;
pub use error::AudiobookError;
pub use probe::{Confidence, MediaProbe};
pub use title::{PartSource, Title};
