//! Chapters and parts — format-independent, and honest about their provenance.

use alloc::string::String;
use alloc::vec::Vec;

/// Where a chapter list came from.
///
/// This is carried on the model rather than discarded, because a library needs
/// to know it is looking at a *declared* chapter list versus one a tool
/// synthesised. Telling a user "Chapter 12" when the file said nothing is a
/// small lie that a good library does not tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ChapterSource {
    /// The container declared these chapters (`CHAP`/`CTOC`, `chpl`, or a
    /// QuickTime text track).
    #[default]
    Declared,
    /// The chapters were inferred from track boundaries, not declared.
    Inferred,
    /// No chapter information was present.
    None,
}

impl ChapterSource {
    /// Whether the chapters can be trusted as the author's intent.
    #[must_use]
    pub fn is_declared(self) -> bool {
        matches!(self, ChapterSource::Declared)
    }
}

/// One chapter: a title and a start offset.
///
/// The end is derived from the next chapter's start rather than stored, which
/// is what both `CHAP` and `chpl` mean in practice and what avoids two
/// disagreeing numbers for one boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    /// The chapter's title. May be empty — a chapter with no name is still a
    /// boundary a player can jump to.
    pub title: String,
    /// Start offset in milliseconds from the beginning of the *title*, not of
    /// the file the chapter came from.
    pub start_ms: u64,
}

impl Chapter {
    /// A chapter at `start_ms`.
    #[must_use]
    pub fn new(title: &str, start_ms: u64) -> Self {
        Self {
            title: String::from(title),
            start_ms,
        }
    }

    /// A chapter with no title.
    ///
    /// Legal and worth keeping: a file that marks boundaries without naming them
    /// still navigates correctly, and inventing "Track 4" would misrepresent
    /// the source.
    #[must_use]
    pub fn untitled(start_ms: u64) -> Self {
        Self {
            title: String::new(),
            start_ms,
        }
    }

    /// The duration to the next chapter, or `None` for the last one.
    #[must_use]
    pub fn duration_ms(&self, next_start_ms: u64) -> Option<u64> {
        Some(next_start_ms.saturating_sub(self.start_ms))
    }
}

/// A part: a named section of a title, as a `CTOC` nests them.
///
/// A single-file audiobook usually has one part holding every chapter. A
/// multi-disc or series-as-one-book edition has one part per disc. Modelling
/// parts explicitly means both look the same to a player.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Part {
    /// The part's title. Often empty for a single-part book.
    pub title: String,
    /// The chapters in this part, in playing order.
    pub chapters: Vec<Chapter>,
    /// Where this part starts within the whole title, in milliseconds.
    ///
    /// Zero for a single-file book. Set by [`crate::Title::laid_out`], because
    /// only the sum of the preceding parts' lengths determines it — no container
    /// states it.
    pub offset_ms: u64,
    /// The part's own length, when the container or the caller knows it.
    ///
    /// Distinct from the span implied by the last chapter's start, which is only
    /// a *lower bound*: nothing in a chapter list says where the final chapter
    /// ends. A multi-file title does know — the host read the files and can
    /// measure them — and without this field every part whose first chapter
    /// starts at 0 would measure as zero length.
    pub duration_ms: Option<u64>,
}

impl Part {
    /// An empty part of unknown length.
    #[must_use]
    pub fn new(title: &str) -> Self {
        Self {
            title: String::from(title),
            chapters: Vec::new(),
            offset_ms: 0,
            duration_ms: None,
        }
    }

    /// An empty part whose length is known.
    #[must_use]
    pub fn with_duration(title: &str, duration_ms: u64) -> Self {
        Self {
            title: String::from(title),
            chapters: Vec::new(),
            offset_ms: 0,
            duration_ms: Some(duration_ms),
        }
    }

    /// The lower bound on the part's length, taken from its last chapter's start.
    ///
    /// `None` when the part has no chapters. This is a *bound*, not a duration:
    /// the last chapter runs to the end of the audio and nothing declares where,
    /// so the true length is this plus the final chapter's length.
    #[must_use]
    pub fn implied_span_ms(&self) -> Option<u64> {
        self.chapters.last().map(|c| c.start_ms)
    }

    /// The part's length: the declared duration when there is one, else the
    /// implied lower bound.
    ///
    /// `0` for a part with neither, which keeps arithmetic total at the cost of
    /// having to say `0` rather than `None` — a caller laying parts end to end
    /// needs a number, not an option.
    #[must_use]
    pub fn effective_duration_ms(&self) -> u64 {
        self.duration_ms
            .or_else(|| self.implied_span_ms())
            .unwrap_or(0)
    }

    /// Chapters with durations filled in from the next chapter's start.
    #[must_use]
    pub fn chapters_with_durations(&self) -> Vec<(u64, Option<u64>, String)> {
        let mut out = Vec::with_capacity(self.chapters.len());
        for (i, c) in self.chapters.iter().enumerate() {
            let dur = self.chapters.get(i + 1).map(|n| n.start_ms);
            out.push((c.start_ms, dur.map(|n| n - c.start_ms), c.title.clone()));
        }
        out
    }
}

/// Concatenate per-file chapter lists into one for a title.
///
/// Each file's chapters are offset by the running total of the files before it,
/// which is the whole reason a multi-file MP3 book needs assembly at all. Files
/// with no declared chapters contribute their duration as a single untitled
/// boundary rather than being skipped — a player that jumps to "file 4" is
/// better than one that has no entry for it at all.
///
/// The offset is what makes this correct rather than merely plausible, and it is
/// also why `Chapter::start_ms` is documented as being relative to the *title*.
#[must_use]
pub fn concatenate(files: &[Vec<(u64, String)>]) -> Vec<Chapter> {
    let mut out: Vec<Chapter> = Vec::new();
    let mut offset: u64 = 0;
    for file in files {
        for (start, title) in file {
            out.push(Chapter::new(title, offset.saturating_add(*start)));
        }
        offset = offset.saturating_add(estimate_duration(file));
    }
    out
}

/// Estimate a file's duration from its last chapter.
///
/// Weak, and documented as such: without a decoded length there is nothing
/// better, and using the last chapter's start under-counts the final chapter.
/// This is why the caller should pass real durations when it has them — see
/// [`concatenate_with_durations`].
#[must_use]
pub fn estimate_duration(chapters: &[(u64, String)]) -> u64 {
    chapters.iter().map(|(ms, _)| *ms).max().unwrap_or(0)
}

/// Concatenate per-file chapters using real per-file durations.
///
/// This is the version to prefer: `durations[i]` is the length of file `i` in
/// milliseconds, which a decoder or a v1 tag's track count can supply.
#[must_use]
pub fn concatenate_with_durations(files: &[Vec<(u64, String)>], durations: &[u64]) -> Vec<Chapter> {
    let mut out: Vec<Chapter> = Vec::new();
    let mut offset: u64 = 0;
    for (i, file) in files.iter().enumerate() {
        for (start, title) in file {
            out.push(Chapter::new(title, offset.saturating_add(*start)));
        }
        offset = offset.saturating_add(durations.get(i).copied().unwrap_or(0));
    }
    out
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
    fn a_chapter_duration_comes_from_the_next_start() {
        let a = Chapter::new("One", 0);
        let b = Chapter::new("Two", 1_000);
        assert_eq!(a.duration_ms(b.start_ms), Some(1_000));
        // The last chapter has no known end.
        assert_eq!(b.duration_ms(5_000), Some(4_000));
    }

    #[test]
    fn an_untitled_chapter_is_legal() {
        let c = Chapter::untitled(500);
        assert_eq!(c.title, "");
        assert_eq!(c.start_ms, 500);
    }

    #[test]
    fn chapters_concatenate_with_a_running_offset() {
        // Two files: the second file's chapters start at 300_000 because the
        // first is that long.
        let files = vec![
            vec![(0u64, String::from("A1")), (150_000, String::from("A2"))],
            vec![(0u64, String::from("B1")), (90_000, String::from("B2"))],
        ];
        let out = concatenate(&files);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].start_ms, 0);
        assert_eq!(out[1].start_ms, 150_000);
        assert_eq!(
            out[2].start_ms, 150_000,
            "file 2's first chapter is offset by file 1"
        );
        assert_eq!(out[3].start_ms, 240_000);
    }

    #[test]
    fn durations_are_preferred_over_estimation() {
        let files = vec![
            vec![(0u64, String::from("A1"))],
            vec![(0u64, String::from("B1"))],
        ];
        // Estimation cannot know file 1 is 10 minutes long.
        let estimated = concatenate(&files);
        assert_eq!(
            estimated[1].start_ms, 0,
            "estimation alone loses the offset"
        );

        let exact = concatenate_with_durations(&files, &[600_000, 300_000]);
        assert_eq!(
            exact[1].start_ms, 600_000,
            "a real duration restores the offset"
        );
    }

    #[test]
    fn a_part_reports_durations_from_its_chapters() {
        let mut p = Part::new("Book One");
        p.chapters.push(Chapter::new("One", 0));
        p.chapters.push(Chapter::new("Two", 60_000));
        p.chapters.push(Chapter::new("Three", 120_000));

        let with_durs = p.chapters_with_durations();
        assert_eq!(with_durs[0], (0, Some(60_000), String::from("One")));
        assert_eq!(with_durs[1], (60_000, Some(60_000), String::from("Two")));
        assert_eq!(
            with_durs[2].1, None,
            "the final chapter has no declared end"
        );
        assert_eq!(p.implied_span_ms(), Some(120_000));
        assert_eq!(p.effective_duration_ms(), 120_000);
    }

    #[test]
    fn a_declared_duration_beats_the_implied_span() {
        let mut p = Part::new("Book One");
        p.chapters.push(Chapter::new("One", 0));
        // The last chapter starts at 0, so the implied span is nothing at all —
        // which is why a declared duration has to be a separate field.
        assert_eq!(p.implied_span_ms(), Some(0));
        assert_eq!(p.effective_duration_ms(), 0);

        p.duration_ms = Some(600_000);
        assert_eq!(p.effective_duration_ms(), 600_000);
        assert_eq!(
            p.implied_span_ms(),
            Some(0),
            "the implied span is unaffected by a declared duration"
        );
    }

    #[test]
    fn an_empty_part_has_no_span() {
        let p = Part::new("Empty");
        assert_eq!(p.implied_span_ms(), None);
        assert_eq!(p.effective_duration_ms(), 0);
        assert!(p.chapters_with_durations().is_empty());
    }

    #[test]
    fn a_part_built_with_a_duration_knows_it() {
        let p = Part::with_duration("Disc One", 3_600_000);
        assert_eq!(p.duration_ms, Some(3_600_000));
        assert_eq!(p.effective_duration_ms(), 3_600_000);
        assert_eq!(p.implied_span_ms(), None, "no chapters, so no implied span");
    }

    #[test]
    fn chapter_source_records_provenance() {
        assert!(ChapterSource::Declared.is_declared());
        assert!(!ChapterSource::Inferred.is_declared());
        assert!(!ChapterSource::None.is_declared());
    }
}

/// `proptest!` must generate a test at module scope: nested inside a `#[test]`
/// function the generated `#[test]` item cannot be collected, so the property
/// silently never runs — which is worse than not having it.
#[cfg(test)]
mod properties {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )]
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use proptest::prelude::*;

    // The nested shape goes through a type alias because a type this complex
    /// written inline in the macro's pattern trips the closure-parsing grammar.
    type ChapterList = Vec<(u64, String)>;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Every declared chapter appears exactly once, at its file's running
        /// offset plus its own start.
        #[test]
        fn offsets_accumulate(
            file_chapter_counts in prop::collection::vec(0usize..4, 0..4),
            per_file_starts in prop::collection::vec(
                prop::collection::vec(0u64..1_000_000, 0..4), 0..4,
            ),
            durations in prop::collection::vec(0u64..10_000_000, 0..4),
        ) {
            // The declared chapter count must be at least the number of starts
            // supplied, or the property has nothing to check.
            prop_assume!(per_file_starts.len() >= file_chapter_counts.len());
            prop_assume!(durations.len() >= file_chapter_counts.len());

            let files: Vec<ChapterList> = file_chapter_counts
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    (0..*n)
                        .map(|j| {
                            let start = per_file_starts[i].get(j).copied().unwrap_or(0);
                            (start, alloc::format!("f{i}c{j}"))
                        })
                        .collect()
                })
                .collect();
            let durs: Vec<u64> = durations.iter().take(files.len()).copied().collect();

            let out = concatenate_with_durations(&files, &durs);

            let mut expected: Vec<(String, u64)> = Vec::new();
            let mut offset = 0u64;
            for (i, file) in files.iter().enumerate() {
                for (start, title) in file {
                    expected.push((title.clone(), offset + start));
                }
                offset += durs[i];
            }
            let actual: Vec<(String, u64)> =
                out.iter().map(|c| (c.title.clone(), c.start_ms)).collect();
            prop_assert_eq!(actual, expected);
        }
    }

    // Concatenation never reorders: the output is the input, in order.
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn concatenation_preserves_input_order(
            files in prop::collection::vec(
                prop::collection::vec((0u64..1_000_000, "[a-z]{0,6}"), 0..4), 0..4,
            ),
        ) {
            let durs = vec![1_000u64; files.len()];
            let out = concatenate_with_durations(&files, &durs);
            let names: Vec<&str> = out.iter().map(|c| c.title.as_str()).collect();
            let flattened: Vec<&str> =
                files.iter().flatten().map(|(_, t)| t.as_str()).collect();
            prop_assert_eq!(names, flattened);
        }
    }
}
