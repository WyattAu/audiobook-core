//! The title: one book, however it was packaged.
//!
//! # Why a title is not a file
//!
//! A library row is a *work*, and the three ways that work reaches disk — one
//! M4B, one tagged MP3, or a folder of MP3s — disagree about almost everything:
//!
//! | | one M4B | one MP3 | folder of MP3s |
//! |---|---|---|---|
//! | chapters | one `chpl` list | one `CHAP` list | one list per file |
//! | chapter times | already absolute | already absolute | per file, needs offsetting |
//! | part structure | usually absent | `CTOC`, often present | one `CTOC` per file |
//! | duration | declared in `mvhd` | not declared at all | sum of per-file lengths |
//!
//! A library that stores whatever each container happens to have has no place to
//! put a row, and cannot sort by title, group by author, or show a duration. This
//! type is that place.
//!
//! # What it will not do
//!
//! **It will not invent structure.** A title with no chapters has
//! [`ChapterSource::None`]. Deriving one chapter per file is tempting and
//! produces a UI that says "Chapter 1 … Chapter 12" for a book whose author
//! wrote nine — a small lie that then gets cached and re-exported. Absence is
//! reported as absence.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::chapter::{Chapter, ChapterSource, Part};
use crate::container::ContainerKind;
use crate::error::AudiobookError;
use crate::probe::{Confidence, MediaProbe};

/// Where the parts came from.
///
/// A single-file book has one implicit part. A folder of files has one part per
/// file. A `CTOC` may declare parts explicitly, and when it does that wins —
/// because the author's own structure beats the file layout's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum PartSource {
    /// One implicit part holding every chapter.
    #[default]
    SingleFile,
    /// One part per source file.
    PerFile,
    /// Parts declared by an ID3 `CTOC`.
    Toc,
}

/// A book: its metadata, its structure, and where the structure came from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Title {
    /// The book's title.
    pub title: String,
    /// The author, or the narrator where the container distinguishes them.
    pub author: String,
    /// The narrator, when the container says so separately.
    pub narrator: String,
    /// The album/series name, when the container carries one.
    pub series: String,
    /// The book's number in its series, when known.
    pub series_index: Option<u32>,
    /// The release year, as written. A string, not a number: containers store
    /// `"1997"`, `"1997-04"`, and `"1997-04-21"`, and collapsing them to a year
    /// loses information the source had.
    pub year: String,
    /// A synopsis or description, when the container carries one.
    pub description: String,
    /// The audio's total duration in milliseconds, when known.
    pub duration_ms: Option<u64>,
    /// How much to trust [`Title::duration_ms`].
    pub confidence: Confidence,
    /// The parts, in playing order.
    pub parts: Vec<Part>,
    /// Where the parts came from.
    pub part_source: PartSource,
    /// Where the chapters came from.
    pub chapter_source: ChapterSource,
    /// The container the title was read from, when it came from one file.
    pub container: Option<ContainerKind>,
    /// Cover art bytes, when present.
    pub cover: Option<Vec<u8>>,
}

impl Title {
    /// An empty title.
    #[must_use]
    pub fn new(title: &str, author: &str) -> Self {
        Self {
            title: String::from(title),
            author: String::from(author),
            ..Self::default()
        }
    }

    /// Add a part.
    #[must_use]
    pub fn with_part(mut self, part: Part) -> Self {
        self.parts.push(part);
        self
    }

    /// Every chapter across every part, in playing order, with times relative to
    /// the **whole title**.
    ///
    /// A part's own [`Part::chapters`] are part-local, which is what a player
    /// seeking inside one file needs; this view rebases each onto the title. Mixing
    /// the two is the mistake a flat list invites, so the absolute one is what the
    /// public API returns and the local one is reached deliberately through the part.
    ///
    /// The result is a projection: a chapter spanning a part boundary appears
    /// **once**, with [`Chapter::parts`] naming every part it covers, because a flat
    /// list that repeated it would make the boundary ambiguous rather than clearer.
    #[must_use]
    pub fn chapters(&self) -> Vec<Chapter> {
        self.parts
            .iter()
            .flat_map(|part| part.chapters.iter())
            .map(|c| {
                let mut rebased = c.clone();
                rebased.start_ms = c.title_start_ms();
                rebased
            })
            .collect()
    }

    /// The chapter playing at `at_ms`, if any.
    ///
    /// Returns the borrowed chapter, whose `start_ms` stays **file-local** — the
    /// offset it was found at is not stored on it, and inventing a copy per call
    /// would allocate on every seek. A caller that needs the absolute number uses
    /// [`Title::chapters`], or adds the offset of
    /// [`Title::part_at`] itself.
    #[must_use]
    pub fn chapter_at(&self, at_ms: u64) -> Option<&Chapter> {
        let mut found: Option<&Chapter> = None;
        for part in &self.parts {
            for c in &part.chapters {
                let absolute = part.offset_ms.saturating_add(c.start_ms);
                if absolute <= at_ms {
                    found = Some(c);
                } else {
                    // The flattened list is in playing order, so the first chapter
                    // past `at_ms` ends the search.
                    return found;
                }
            }
        }
        found
    }

    /// Recompute every part's [`Part::offset_ms`] from the parts' lengths.
    ///
    /// Offsets cannot be known until the parts are in playing order, so assembly
    /// stores the lengths and calls this before handing the title back. Call it
    /// again after reordering the parts.
    ///
    /// It has to be this way round because the alternative — deriving offsets from
    /// chapter times — is wrong for exactly the multi-file case that needs them
    /// most: every part's first chapter starts at zero, so chapter times carry no
    /// information about where a part begins. Only the running sum of the parts'
    /// lengths does.
    #[must_use]
    pub fn laid_out(mut self) -> Self {
        let mut cursor = 0u64;
        for part in &mut self.parts {
            part.offset_ms = cursor;
            cursor = cursor.saturating_add(part.effective_duration_ms());
        }
        // Chapter offsets are *derived* from the part offsets, so they can only be
        // filled in once those are final — which is why this is a second pass rather
        // than something the part's constructor could do.
        //
        // A chapter's offset comes from the part it *starts* in, not the part holding
        // it: a chapter spanning parts 2 and 3 begins in part 2, and its start is part
        // 2's offset plus its part-local time.
        for (i, part) in self.parts.iter_mut().enumerate() {
            let offset = part.offset_ms;
            for c in &mut part.chapters {
                debug_assert!(
                    c.first_part() == i,
                    "a chapter stored in part {i} claims to start in part {}",
                    c.first_part()
                );
                c.set_title_offset(offset);
            }
        }
        self
    }

    /// The parts in playing order, each carrying the offset it was laid out at.
    #[must_use]
    pub fn parts_with_offsets(&self) -> &[Part] {
        &self.parts
    }

    /// Assemble a title from a flat chapter list plus one length per part.
    ///
    /// This is the constructor to reach for when the chapters come from
    /// [`crate::chapter::concatenate_spanning`], because it places **each chapter in
    /// the part it starts in** — which is the invariant every other operation on this
    /// type assumes. Building the parts by hand and then filing a chapter under the
    /// part it merely *spans* is the easy mistake, and the consequence is a chapter
    /// whose whole-title offset comes from the wrong part.
    ///
    /// A chapter naming a part index that does not exist is kept, in the last part,
    /// rather than dropped: losing a boundary silently is the failure this crate
    /// treats as worst.
    #[must_use]
    pub fn from_flat_chapters(
        chapters: Vec<Chapter>,
        durations: &[u64],
        title: &str,
        author: &str,
    ) -> Self {
        let mut parts: Vec<Part> = durations
            .iter()
            .map(|d| Part {
                title: String::new(),
                chapters: Vec::new(),
                offset_ms: 0,
                duration_ms: Some(*d),
            })
            .collect();
        // No durations given: one part per distinct index the chapters name.
        if parts.is_empty() {
            let needed = chapters
                .iter()
                .map(|c| c.first_part() + 1)
                .max()
                .unwrap_or(1);
            parts = (0..needed)
                .map(|_| Part {
                    title: String::new(),
                    chapters: Vec::new(),
                    offset_ms: 0,
                    duration_ms: None,
                })
                .collect();
        }

        let highest = parts.len().saturating_sub(1);
        for mut c in chapters {
            let declared = c.first_part();
            // A chapter naming parts this title does not have has a meaningless
            // span, so it is filed under the last part it *could* mean and its span
            // collapses to that one. Keeping the old span would leave the stored
            // index disagreeing with the part holding it.
            let index = declared.min(highest);
            if declared > highest {
                c.file_under(index);
            }
            match parts.get_mut(index) {
                Some(part) => part.chapters.push(c),
                // Only reachable when `durations` was empty *and* no chapter named
                // a part, which the `needed` fallback above already covers. Kept so
                // the loop cannot panic on a pathological input.
                None => continue,
            }
        }

        let chapter_source = if parts.iter().all(|p| p.chapters.is_empty()) {
            ChapterSource::None
        } else {
            ChapterSource::Declared
        };
        let duration: u64 = durations
            .iter()
            .copied()
            .fold(0u64, |a, b| a.saturating_add(b));

        Self {
            title: String::from(title),
            author: String::from(author),
            duration_ms: if duration > 0 { Some(duration) } else { None },
            confidence: if duration > 0 {
                Confidence::Declared
            } else {
                Confidence::Unknown
            },
            chapter_source,
            parts,
            part_source: PartSource::PerFile,
            ..Self::default()
        }
        .laid_out()
    }

    /// The total chapter count.
    #[must_use]
    pub fn chapter_count(&self) -> usize {
        self.parts.iter().map(|p| p.chapters.len()).sum()
    }

    /// Whether this title has any chapters at all.
    #[must_use]
    pub fn has_chapters(&self) -> bool {
        self.chapter_count() > 0
    }

    /// The duration, falling back to the sum of the parts' lengths and then to
    /// the last chapter's start.
    ///
    /// Each fallback under-counts, because nothing in a chapter list declares
    /// where the final chapter ends. It is better than no duration at all, and
    /// [`Title::confidence`] stays [`Confidence::Unknown`] when a fallback was
    /// used so a caller can tell.
    #[must_use]
    pub fn duration_ms(&self) -> Option<u64> {
        if let Some(d) = self.duration_ms {
            return Some(d);
        }
        let summed: u64 = self
            .parts
            .iter()
            .map(Part::effective_duration_ms)
            .fold(0u64, |a, b| a.saturating_add(b));
        if summed > 0 {
            return Some(summed);
        }
        self.parts
            .iter()
            .filter_map(Part::implied_span_ms)
            .max()
            .filter(|ms| *ms > 0)
    }

    /// Whether the reported duration is a count rather than an estimate.
    #[must_use]
    pub fn has_exact_duration(&self) -> bool {
        self.duration_ms.is_some() && self.confidence == Confidence::Declared
    }

    /// Assemble a title from a probed single file.
    ///
    /// The file's own metadata wins over the arguments, because a container that
    /// states a title should not have one overwritten by a filename. The
    /// arguments are the fallback for when it does not — which is the common case
    /// for a hand-made book.
    #[must_use]
    pub fn from_probe(probe: &MediaProbe, fallback_title: &str, fallback_author: &str) -> Self {
        let title = probe
            .title
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| String::from(fallback_title));
        let author = probe
            .artist
            .clone()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| String::from(fallback_author));

        let chapter_source = if probe.chapters.is_empty() {
            ChapterSource::None
        } else {
            ChapterSource::Declared
        };

        // One file, one part — but only when there are chapters to put in it. A
        // title with no chapters and one empty part is noise in a library's
        // structure view, so the parts list is left empty instead.
        let parts: Vec<Part> = if chapter_source == ChapterSource::None {
            Vec::new()
        } else {
            vec![Part {
                title: String::new(),
                chapters: probe
                    .chapters
                    .iter()
                    .map(|(ms, t)| Chapter::new(t, *ms, 0))
                    .collect(),
                // A single file's length is the probe's own duration when it had
                // one; without it the part falls back to the implied span.
                duration_ms: probe.duration_ms,
                offset_ms: 0,
            }]
        };

        Self {
            title,
            author,
            series: probe.album.clone().unwrap_or_default(),
            year: probe.year.clone().unwrap_or_default(),
            duration_ms: probe.duration_ms,
            confidence: probe.confidence,
            parts,
            part_source: PartSource::SingleFile,
            chapter_source,
            container: probe.container,
            cover: probe.cover.clone(),
            ..Self::default()
        }
        // A single part starts at zero, but laying out costs nothing and keeps
        // every constructor on the same invariant: `offset_ms` is always the
        // running sum of the preceding lengths.
        .laid_out()
    }

    /// Assemble a title from several probed files, concatenating their chapters.
    ///
    /// `durations[i]` is file `i`'s length in milliseconds; a real value per
    /// file is what makes the concatenated chapter times correct, and passing an
    /// empty slice is accepted but yields offsets that are only as good as each
    /// file's own last chapter.
    #[must_use]
    pub fn from_probes(
        probes: &[MediaProbe],
        durations: &[u64],
        fallback_title: &str,
        fallback_author: &str,
    ) -> Self {
        let per_file: Vec<Vec<(u64, String)>> = probes.iter().map(|p| p.chapters.clone()).collect();

        let chapters = crate::chapter::concatenate(&per_file);
        let chapter_source = if chapters.is_empty() {
            ChapterSource::None
        } else {
            ChapterSource::Declared
        };

        // Duration: the sum of real per-file durations when given, else the
        // total of the probes' own reported durations.
        let declared: Option<u64> = if durations.len() == probes.len() && !durations.is_empty() {
            Some(
                durations
                    .iter()
                    .copied()
                    .fold(0u64, |a, b| a.saturating_add(b)),
            )
        } else {
            probes
                .iter()
                .filter_map(|p| p.duration_ms)
                .reduce(|a, b| a.saturating_add(b))
        };
        let confidence = if durations.len() == probes.len() && !durations.is_empty() {
            Confidence::Declared
        } else {
            probes
                .iter()
                .map(|p| p.confidence)
                .max()
                .unwrap_or(Confidence::Unknown)
        };

        let title = probes
            .iter()
            .find_map(|p| p.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| String::from(fallback_title));
        let author = probes
            .iter()
            .find_map(|p| p.artist.clone())
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| String::from(fallback_author));

        // One part per file, which is the honest structure: a folder of ten MP3s
        // is ten files, and merging them into one part would hide that a player's
        // file list and this library's part list mean different things.
        let parts: Vec<Part> = if chapter_source == ChapterSource::None {
            Vec::new()
        } else {
            // The probe index is kept across the filter on purpose: dropping a
            // file must not shift the remaining files onto the wrong durations.
            probes
                .iter()
                .enumerate()
                .filter(|(_, p)| !p.chapters.is_empty())
                .map(|(i, p)| Part {
                    title: p
                        .album
                        .clone()
                        .filter(|a| !a.is_empty())
                        .unwrap_or_default(),
                    // Part-local chapter times, which is what a player seeking
                    // within one file needs. `Title::chapters()` returns
                    // title-absolute times for a whole-book seek bar.
                    chapters: p
                        .chapters
                        .iter()
                        .map(|(ms, t)| Chapter::new(t, *ms, i))
                        .collect(),
                    // The file's own length, when the caller supplied one. This
                    // is what lets `part_at` say which file is playing: a part
                    // whose chapters start at 0 spans nothing on its own.
                    duration_ms: durations.get(i).copied().or(p.duration_ms),
                    offset_ms: 0,
                })
                .collect()
        };

        Self {
            title,
            author,
            series: probes
                .first()
                .and_then(|p| p.album.clone())
                .unwrap_or_default(),
            year: probes
                .iter()
                .find_map(|p| p.year.clone())
                .unwrap_or_default(),
            duration_ms: declared,
            confidence,
            parts,
            part_source: PartSource::PerFile,
            chapter_source,
            container: probes.first().and_then(|p| p.container),
            cover: probes.iter().find_map(|p| p.cover.clone()),
            ..Self::default()
        }
        // Without this every part after the first would report its chapters at
        // offset 0, so a seek bar would jump to the wrong file.
        .laid_out()
    }

    /// Set the narrator separately from the author.
    #[must_use]
    pub fn with_narrator(mut self, narrator: &str) -> Self {
        self.narrator = String::from(narrator);
        self
    }

    /// Set the series and index.
    #[must_use]
    pub fn with_series(mut self, series: &str, index: u32) -> Self {
        self.series = String::from(series);
        self.series_index = Some(index);
        self
    }

    /// Set a synopsis.
    #[must_use]
    pub fn with_description(mut self, description: &str) -> Self {
        self.description = String::from(description);
        self
    }

    /// The part containing `at_ms`, if any.
    #[must_use]
    pub fn part_at(&self, at_ms: u64) -> Option<&Part> {
        let mut cursor = 0u64;
        for part in &self.parts {
            let span = part.effective_duration_ms();
            // Half-open, so a time exactly on a boundary belongs to the next
            // part rather than to both.
            if at_ms < cursor.saturating_add(span) {
                return Some(part);
            }
            cursor = cursor.saturating_add(span);
        }
        None
    }

    /// Check the title's internal consistency.
    ///
    /// Three ways a title can be internally inconsistent, all of which come from
    /// a real producer rather than from a bug here:
    ///
    /// - chapters out of order or duplicated at the same offset;
    /// - a chapter starting past the end of the audio;
    /// - two parts claiming the same name.
    ///
    /// # Errors
    ///
    /// [`AudiobookError::ChaptersOverlap`], [`AudiobookError::ChapterPastEnd`],
    /// or [`AudiobookError::DuplicatePart`].
    pub fn validate(&self) -> Result<(), AudiobookError> {
        let mut seen_parts: Vec<&str> = Vec::new();
        for part in &self.parts {
            if !part.title.is_empty() {
                if seen_parts.contains(&part.title.as_str()) {
                    return Err(AudiobookError::DuplicatePart {
                        title: part.title.clone(),
                    });
                }
                seen_parts.push(part.title.as_str());
            }

            let mut previous: Option<&Chapter> = None;
            for c in &part.chapters {
                if let Some(prev) = previous {
                    if c.start_ms < prev.start_ms {
                        return Err(AudiobookError::ChaptersOverlap {
                            first: prev.title.clone(),
                            second: c.title.clone(),
                            at_ms: c.start_ms,
                        });
                    }
                    if c.start_ms == prev.start_ms && c.title != prev.title {
                        // Two different chapters at the same instant make the
                        // order arbitrary, which is what a seek bar needs to be
                        // unambiguous.
                        return Err(AudiobookError::ChaptersOverlap {
                            first: prev.title.clone(),
                            second: c.title.clone(),
                            at_ms: c.start_ms,
                        });
                    }
                }
                previous = Some(c);
            }
        }

        if let Some(duration) = self.duration_ms {
            for part in &self.parts {
                for c in &part.chapters {
                    if c.start_ms > duration {
                        return Err(AudiobookError::ChapterPastEnd {
                            title: c.title.clone(),
                            start_ms: c.start_ms,
                            duration_ms: duration,
                        });
                    }
                }
            }
        }

        Ok(())
    }

    /// Move a part to a new position, then re-lay out the offsets.
    ///
    /// Ordering a folder's files is the host's decision — this crate sees bytes,
    /// not filenames — so the operation takes an index rather than comparing
    /// names. A lexical sort of `10.mp3, 2.mp3` needs exactly this: put part 1
    /// first, and every offset after it moves.
    ///
    /// Out-of-range indices are ignored rather than panicking, because a caller
    /// deriving an index from a file listing should not be able to abort a
    /// library's import.
    #[must_use]
    pub fn with_part_at(mut self, from: usize, to: usize) -> Self {
        if from >= self.parts.len() || to >= self.parts.len() || from == to {
            return self;
        }
        let part = self.parts.remove(from);
        self.parts.insert(to, part);

        // Renumber every chapter's part references, because a chapter names a part
        // by index and a stale index points at a real but wrong file. This is the
        // price of storing indices rather than back-pointers, and paying it here is
        // cheaper than having every consumer of `Chapter::parts` check.
        let count = self.parts.len();
        let mut mapping = vec![count; count];
        let mut old = 0usize;
        while old < count {
            if old == from {
                old += 1;
                continue;
            }
            let new = if old < from { old } else { old - 1 };
            let moved = if new >= to { new + 1 } else { new };
            if let Some(slot) = mapping.get_mut(old) {
                *slot = moved;
            }
            old += 1;
        }
        if let Some(slot) = mapping.get_mut(from) {
            *slot = to;
        }

        for part in &mut self.parts {
            for c in &mut part.chapters {
                c.remap_parts(&mapping);
            }
        }

        self.laid_out()
    }

    /// Whether two titles are the same work, judged by metadata alone.
    ///
    /// Deliberately conservative and case-insensitive: a trimmed, ASCII
    /// case-folded title **and** author must both agree, and the series is
    /// ignored, because two books with the same title by different authors are
    /// common while two editions of one work routinely disagree about their
    /// series. An unknown author on either side never matches, so a folder of
    /// untagged files does not collapse into one row.
    #[must_use]
    pub fn matches(&self, other: &Title) -> bool {
        // Both sides are trimmed. Trimming only the left one compares a trimmed
        // string against an untrimmed one, which silently fails on exactly the
        // sloppy input this function exists to absorb.
        let eq = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
        if !eq(&self.title, &other.title) {
            return false;
        }
        // An unknown author on either side must not silently match everything
        // with the same title.
        if self.author.is_empty() || other.author.is_empty() {
            return false;
        }
        eq(&self.author, &other.author)
    }
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

    fn probed(chapters: &[(u64, &str)], duration_ms: Option<u64>) -> MediaProbe {
        MediaProbe {
            duration_ms,
            confidence: if duration_ms.is_some() {
                Confidence::Declared
            } else {
                Confidence::Unknown
            },
            chapters: chapters
                .iter()
                .map(|(ms, t)| (*ms, String::from(*t)))
                .collect(),
            ..MediaProbe::default()
        }
    }

    #[test]
    fn a_title_with_chapters_answers_a_seek_query() {
        let title = Title::new("Book", "Author").with_part(Part {
            title: String::new(),
            chapters: vec![
                Chapter::new("One", 0, 0),
                Chapter::new("Two", 60_000, 0),
                Chapter::new("Three", 120_000, 0),
            ],
            offset_ms: 0,
            duration_ms: Some(180_000),
        });
        assert_eq!(title.chapter_count(), 3);
        assert_eq!(title.chapter_at(0).map(|c| c.title.as_str()), Some("One"));
        assert_eq!(
            title.chapter_at(60_000).map(|c| c.title.as_str()),
            Some("Two")
        );
        assert_eq!(
            title.chapter_at(119_999).map(|c| c.title.as_str()),
            Some("Two"),
            "the interval is half-open, so 119_999 is still inside chapter two"
        );
        assert_eq!(
            title.chapter_at(60_000).map(|c| c.title.as_str()),
            Some("Two"),
            "a chapter's start belongs to that chapter, not the previous one"
        );
        assert_eq!(
            title.chapter_at(500_000).map(|c| c.title.as_str()),
            Some("Three"),
            "the last chapter runs to the end of the audio"
        );
    }

    #[test]
    fn a_title_with_no_chapters_says_so() {
        let title = Title::from_probe(&probed(&[], None), "file.mp3", "unknown");
        assert_eq!(title.title, "file.mp3");
        assert_eq!(title.chapter_source, ChapterSource::None);
        assert!(!title.has_chapters());
        assert!(title.parts.is_empty(), "an empty part is not a part");
        assert_eq!(title.chapter_at(0), None);
    }

    #[test]
    fn a_container_title_beats_the_filename() {
        let mut probe = probed(&[(0, "Opening")], Some(60_000));
        probe.title = Some(String::from("Real Title"));
        probe.artist = Some(String::from("Real Author"));

        let title = Title::from_probe(&probe, "book01.mp3", "nobody");
        assert_eq!(title.title, "Real Title", "the container wins");
        assert_eq!(title.author, "Real Author");
    }

    #[test]
    fn an_empty_container_title_falls_back_to_the_filename() {
        let mut probe = probed(&[(0, "Opening")], Some(60_000));
        probe.title = Some(String::new());
        let title = Title::from_probe(&probe, "book01.mp3", "nobody");
        assert_eq!(title.title, "book01.mp3", "an empty string is not a title");
    }

    #[test]
    fn a_single_file_probe_becomes_one_part() {
        let probe = probed(&[(0, "One"), (1_000, "Two")], Some(10_000));
        let title = Title::from_probe(&probe, "x", "y");
        assert_eq!(title.parts.len(), 1);
        assert_eq!(title.part_source, PartSource::SingleFile);
        assert_eq!(title.chapter_source, ChapterSource::Declared);
        assert_eq!(title.duration_ms(), Some(10_000));
        assert!(title.has_exact_duration());
    }

    #[test]
    fn multi_file_chapters_are_offset_by_real_durations() {
        let probes = vec![
            probed(&[(0, "A1"), (1_000, "A2")], Some(600_000)),
            probed(&[(0, "B1")], Some(300_000)),
        ];
        let title = Title::from_probes(&probes, &[600_000, 300_000], "Book", "Author");

        assert_eq!(title.chapter_source, ChapterSource::Declared);
        assert_eq!(title.part_source, PartSource::PerFile);
        assert_eq!(title.parts.len(), 2, "one part per file");
        assert_eq!(title.duration_ms(), Some(900_000));
        assert_eq!(title.chapter_count(), 3);

        // Part-local times stay relative to their own file, which is what a
        // player seeking within one file needs.
        assert_eq!(title.parts[0].chapters[1].start_ms, 1_000);
        assert_eq!(title.parts[1].chapters[0].start_ms, 0);
    }

    #[test]
    fn validation_accepts_a_well_formed_title() {
        let title = Title::new("Book", "Author")
            .with_part(Part {
                title: String::from("Disc One"),
                chapters: vec![Chapter::new("One", 0, 0), Chapter::new("Two", 60_000, 0)],
                offset_ms: 0,
                duration_ms: Some(60_000),
            })
            .with_part(Part {
                title: String::from("Disc Two"),
                chapters: vec![Chapter::new("Three", 0, 0)],
                offset_ms: 0,
                duration_ms: Some(30_000),
            });
        assert!(title.validate().is_ok());
    }

    #[test]
    fn validation_catches_out_of_order_chapters() {
        let title = Title::new("Book", "Author").with_part(Part {
            title: String::new(),
            chapters: vec![Chapter::new("Two", 60_000, 0), Chapter::new("One", 0, 0)],
            offset_ms: 0,
            duration_ms: None,
        });
        assert!(matches!(
            title.validate(),
            Err(AudiobookError::ChaptersOverlap { .. })
        ));
    }

    #[test]
    fn validation_catches_two_chapters_at_the_same_instant() {
        // Same start, different names: the order is arbitrary, so a seek bar
        // cannot be built.
        let title = Title::new("Book", "Author").with_part(Part {
            title: String::new(),
            chapters: vec![Chapter::new("One", 0, 0), Chapter::new("Two", 0, 0)],
            offset_ms: 0,
            duration_ms: None,
        });
        assert!(matches!(
            title.validate(),
            Err(AudiobookError::ChaptersOverlap { .. })
        ));
    }

    #[test]
    fn validation_allows_two_untitled_chapters_at_the_same_instant() {
        // Both empty: indistinguishable, so nothing is ambiguous.
        let title = Title::new("Book", "Author").with_part(Part {
            title: String::new(),
            chapters: vec![Chapter::untitled(0, 0), Chapter::untitled(0, 0)],
            offset_ms: 0,
            duration_ms: None,
        });
        assert!(title.validate().is_ok());
    }

    #[test]
    fn validation_catches_a_chapter_past_the_end() {
        let title = Title {
            duration_ms: Some(10_000),
            confidence: Confidence::Declared,
            ..Title::new("Book", "Author")
        }
        .with_part(Part {
            title: String::new(),
            chapters: vec![Chapter::new("Way Out", 999_000, 0)],
            offset_ms: 0,
            duration_ms: None,
        });
        assert!(matches!(
            title.validate(),
            Err(AudiobookError::ChapterPastEnd { .. })
        ));
    }

    #[test]
    fn validation_catches_two_parts_with_the_same_name() {
        let title = Title::new("Book", "Author")
            .with_part(Part::new("Disc One"))
            .with_part(Part::new("Disc One"));
        assert!(matches!(
            title.validate(),
            Err(AudiobookError::DuplicatePart { .. })
        ));
    }

    #[test]
    fn laying_out_fills_in_offsets_from_the_parts_lengths() {
        // A part's first chapter always starts at 0, so chapter times cannot
        // supply the offsets — only the running sum of the parts' lengths can.
        let title = Title::new("Book", "Author")
            .with_part(Part {
                title: String::from("Disc One"),
                chapters: vec![Chapter::new("One", 0, 0)],
                offset_ms: 0,
                duration_ms: Some(60_000),
            })
            .with_part(Part {
                title: String::from("Disc Two"),
                chapters: vec![Chapter::new("Two", 0, 1)],
                offset_ms: 0,
                duration_ms: Some(30_000),
            })
            .laid_out();

        assert_eq!(title.parts[0].offset_ms, 0);
        assert_eq!(title.parts[1].offset_ms, 60_000);
        // The flat view is title-absolute; the part view stays file-local.
        assert_eq!(title.chapters()[1].start_ms, 60_000);
        assert_eq!(title.parts[1].chapters[0].start_ms, 0);
    }

    #[test]
    fn moving_a_part_re_derives_every_offset_after_it() {
        let title = Title::new("Book", "Author")
            .with_part(Part {
                title: String::from("A"),
                chapters: vec![Chapter::new("a", 0, 0)],
                offset_ms: 0,
                duration_ms: Some(10_000),
            })
            .with_part(Part {
                title: String::from("B"),
                chapters: vec![Chapter::new("b", 0, 1)],
                offset_ms: 0,
                duration_ms: Some(20_000),
            })
            .with_part(Part {
                title: String::from("C"),
                chapters: vec![Chapter::new("c", 0, 2)],
                offset_ms: 0,
                duration_ms: Some(30_000),
            })
            .laid_out();
        assert_eq!(title.parts[2].offset_ms, 30_000);

        let moved = title.with_part_at(2, 0);
        let names: Vec<&str> = moved.parts.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(names, vec!["C", "A", "B"]);
        assert_eq!(moved.parts[0].offset_ms, 0);
        assert_eq!(moved.parts[1].offset_ms, 30_000, "C is 30 s long");
        assert_eq!(moved.parts[2].offset_ms, 40_000, "then A's 10 s");

        // The move renumbers the chapters, because a chapter names its part by
        // index and a stale index points at a real but wrong file.
        assert_eq!(moved.parts[0].chapters[0].first_part(), 0);
        assert_eq!(moved.parts[0].chapters[0].title, "c");
        assert_eq!(moved.parts[1].chapters[0].first_part(), 1);
        assert_eq!(moved.parts[2].chapters[0].first_part(), 2);
    }

    #[test]
    fn a_flat_chapter_list_is_filed_under_the_part_it_starts_in() {
        // The three-level shape real tooling uses: a chapter that begins in part 0 and
        // runs through part 2. Filing it under the part it merely *spans* would give
        // it part 2's offset, which is wrong by the length of two files.
        use alloc::format;
        let chapters = alloc::vec![
            Chapter::spanning("Long", 0, &[0, 1, 2]),
            Chapter::new("Tail", 0, 2),
        ];
        let title =
            Title::from_flat_chapters(chapters, &[600_000, 600_000, 300_000], "Book", "Author");

        assert_eq!(title.parts.len(), 3);
        assert_eq!(title.parts[0].chapters.len(), 1, "it lives in part 0");
        assert_eq!(title.parts[0].chapters[0].title, "Long");
        assert_eq!(title.parts[2].chapters.len(), 1, "the tail is part 2's");

        // Its whole-title start comes from part 0, not from where the list is stored.
        let flat = title.chapters();
        assert_eq!(flat[0].start_ms, 0);
        assert_eq!(flat[0].parts, alloc::vec![0, 1, 2]);
        assert_eq!(flat[1].start_ms, 1_200_000, "1200 s is the end of part 1");
        assert_eq!(title.duration_ms(), Some(1_500_000));
        let _ = format!("{:?}", flat.len());
    }

    #[test]
    fn a_chapter_naming_a_part_that_does_not_exist_is_kept_not_dropped() {
        // Losing a boundary silently is the worst failure, so an out-of-range
        // chapter lands in the last part where it is at least reachable.
        let chapters = alloc::vec![Chapter::new("Real", 0, 0), Chapter::new("Ghost", 5, 9)];
        let title = Title::from_flat_chapters(chapters, &[1_000], "Book", "Author");
        assert_eq!(title.chapter_count(), 2, "nothing is dropped");
        assert_eq!(title.parts[0].chapters[0].title, "Real");
        assert_eq!(title.parts[0].chapters[1].title, "Ghost");
    }

    #[test]
    fn a_flat_chapter_list_with_no_durations_still_assembles() {
        let chapters = alloc::vec![
            Chapter::new("A", 0, 0),
            Chapter::new("B", 0, 1),
            Chapter::new("C", 0, 2),
        ];
        let title = Title::from_flat_chapters(chapters, &[], "Book", "Author");
        assert_eq!(
            title.parts.len(),
            3,
            "one part per part index the chapters name"
        );
        assert_eq!(title.chapter_count(), 3);
        assert_eq!(title.duration_ms(), None, "no lengths were supplied");
        assert!(!title.has_exact_duration());
    }

    #[test]
    fn an_empty_flat_chapter_list_declares_no_chapters() {
        let title = Title::from_flat_chapters(alloc::vec![], &[1_000], "Book", "Author");
        assert_eq!(title.chapter_source, ChapterSource::None);
        assert_eq!(title.chapter_count(), 0);
        assert_eq!(title.duration_ms(), Some(1_000));
    }

    #[test]
    fn moving_a_part_out_of_range_is_ignored() {
        // A caller deriving an index from a file listing must not be able to
        // abort an import.
        let base = || {
            Title::new("Book", "Author")
                .with_part(Part::with_duration("Only", 1_000))
                .laid_out()
        };
        assert_eq!(base().with_part_at(0, 5).parts[0].title, "Only");
        assert_eq!(base().with_part_at(5, 0).parts[0].title, "Only");
        assert_eq!(base().with_part_at(0, 0).parts[0].title, "Only");
    }

    #[test]
    fn a_duration_falls_back_to_the_last_chapter_but_stays_imprecise() {
        let title = Title::new("Book", "Author").with_part(Part {
            title: String::new(),
            chapters: vec![Chapter::new("One", 0, 0), Chapter::new("Two", 90_000, 0)],
            offset_ms: 0,
            duration_ms: None,
        });
        assert_eq!(title.duration_ms(), Some(90_000));
        assert!(
            !title.has_exact_duration(),
            "a chapter-derived duration is not a declared one"
        );
        assert_eq!(title.confidence, Confidence::Unknown);
    }

    #[test]
    fn a_declared_duration_is_never_overwritten_by_a_chapter_span() {
        let title = Title {
            duration_ms: Some(95_000),
            confidence: Confidence::Declared,
            ..Title::new("Book", "Author")
        }
        .with_part(Part {
            title: String::new(),
            chapters: vec![Chapter::new("One", 90_000, 0)],
            offset_ms: 0,
            duration_ms: None,
        });
        assert_eq!(title.duration_ms(), Some(95_000));
        assert!(title.has_exact_duration());
    }

    #[test]
    fn matching_is_case_insensitive_and_refuses_to_guess() {
        let a = Title::new("The Long Now", "A. Writer");
        let b = Title::new("  the long now ", "a. writer");
        assert!(a.matches(&b));

        let different_author = Title::new("The Long Now", "B. Writer");
        assert!(!a.matches(&different_author));

        // An unknown author must not match everything sharing a title.
        let unknown = Title::new("The Long Now", "");
        assert!(!a.matches(&unknown));
        assert!(!unknown.matches(&a));

        // Nor should the series be what decides — two editions of one work.
        let mut series_edition = a.clone();
        series_edition.series = String::from("Different Series");
        assert!(a.matches(&series_edition));
    }

    #[test]
    fn matching_rejects_a_different_title() {
        assert!(!Title::new("A", "X").matches(&Title::new("B", "X")));
    }

    #[test]
    fn part_lookup_walks_cumulative_spans() {
        let title = Title::new("Book", "Author")
            .with_part(Part {
                title: String::from("Disc One"),
                chapters: vec![Chapter::new("One", 0, 0), Chapter::new("Two", 60_000, 0)],
                offset_ms: 0,
                duration_ms: Some(60_000),
            })
            .with_part(Part {
                title: String::from("Disc Two"),
                chapters: vec![Chapter::new("Three", 0, 0)],
                offset_ms: 0,
                duration_ms: Some(30_000),
            });
        assert_eq!(title.part_at(0).map(|p| p.title.as_str()), Some("Disc One"));
        assert_eq!(
            title.part_at(59_000).map(|p| p.title.as_str()),
            Some("Disc One")
        );
        assert_eq!(
            title.part_at(60_000).map(|p| p.title.as_str()),
            Some("Disc Two")
        );
    }

    #[test]
    fn a_narrator_is_distinct_from_an_author() {
        let title = Title::new("Book", "Author").with_narrator("Reader");
        assert_eq!(title.author, "Author");
        assert_eq!(title.narrator, "Reader");
    }

    #[test]
    fn series_and_index_are_optional_together() {
        let title = Title::new("Book", "Author").with_series("Discworld", 3);
        assert_eq!(title.series, "Discworld");
        assert_eq!(title.series_index, Some(3));

        let plain = Title::new("Book", "Author");
        assert_eq!(plain.series_index, None);
    }

    #[test]
    fn a_description_round_trips() {
        let title = Title::new("Book", "Author").with_description("A synopsis.");
        assert_eq!(title.description, "A synopsis.");
    }

    #[test]
    fn multi_file_with_no_durations_still_reports_a_duration() {
        // Without per-file durations the probes' own reported values are summed.
        let probes = vec![
            probed(&[(0, "A1")], Some(600_000)),
            probed(&[(0, "B1")], Some(300_000)),
        ];
        let title = Title::from_probes(&probes, &[], "Book", "Author");
        assert_eq!(title.duration_ms(), Some(900_000));
    }

    #[test]
    fn an_empty_probe_list_yields_an_empty_title() {
        let title = Title::from_probes(&[], &[], "Folder", "Author");
        assert_eq!(title.title, "Folder");
        assert_eq!(title.author, "Author");
        assert_eq!(title.chapter_source, ChapterSource::None);
        assert_eq!(title.duration_ms(), None);
    }
}
