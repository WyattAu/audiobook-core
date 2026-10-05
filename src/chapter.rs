//! Chapters and parts — format-independent, and honest about their provenance.

use alloc::string::String;
use alloc::vec;
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
/// # Why a chapter spans parts
///
/// A chapter is not a file. A real book's chapter boundary frequently falls in the
/// middle of a file — Audible and Librivox split on time, not on file, so a
/// 14-minute chapter can start 3 minutes into an 18-minute MP3 — and one chapter
/// can run across two files entirely. Tools that build M4Bs express this with a
/// three-level structure of **parts → chapters → files**, where a chapter names
/// several source files.
///
/// A model where a chapter lives inside exactly one file cannot represent that, and
/// the failure is worse than a missing feature: the boundary is either dropped or
/// attributed to the wrong file, and a seek lands in the wrong place with no error
/// anywhere. So the span is part of the chapter rather than implied.
///
/// # Units
///
/// [`Chapter::start_ms`] is **part-local** — relative to the start of
/// [`Chapter::first_part`] — because that is what a per-file container states and
/// what a seek within that file needs. [`Chapter::title_start_ms`] is the
/// whole-title offset, which nothing in a per-file container carries and which
/// [`crate::Title::laid_out`] computes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    /// The chapter's title. May be empty — a chapter with no name is still a
    /// boundary a player can jump to.
    pub title: String,
    /// Start offset in milliseconds from the beginning of
    /// [`Chapter::first_part`], not from the beginning of the title.
    pub start_ms: u64,
    /// The parts this chapter occupies, in playing order.
    ///
    /// Never empty in a model built by this crate: [`Chapter::spanning`] puts a
    /// zero in there if handed nothing, because a chapter belonging to no file is
    /// not representable and dropping it would lose a boundary silently.
    pub parts: Vec<usize>,
    /// The chapter's whole-title offset, filled in by
    /// [`crate::Title::laid_out`].
    ///
    /// Private because zero is both the pre-layout value and the correct value for
    /// a chapter at the very start of a title, so the field cannot describe itself.
    /// Read [`Chapter::title_start_ms`].
    title_offset_ms: u64,
}

impl Chapter {
    /// A chapter at `start_ms` within one file.
    ///
    /// `part` is an index into the title's parts. Single-file chapters are the
    /// common case, so this is the constructor to reach for; a chapter spanning a
    /// boundary is [`Chapter::spanning`].
    #[must_use]
    pub fn new(title: &str, start_ms: u64, part: usize) -> Self {
        Self {
            title: String::from(title),
            start_ms,
            parts: vec![part],
            title_offset_ms: 0,
        }
    }

    /// A chapter occupying `parts` in order, starting `start_ms` into the first.
    ///
    /// The later parts are entered at their own start, which is what concatenation
    /// does. `parts` lists **all** of them including the first — a list of only the
    /// *subsequent* parts would be a different and much more error-prone shape.
    #[must_use]
    pub fn spanning(title: &str, start_ms: u64, parts: &[usize]) -> Self {
        let mut spans: Vec<usize> = parts.to_vec();
        if spans.is_empty() {
            spans.push(0);
        }
        Self {
            title: String::from(title),
            start_ms,
            parts: spans,
            title_offset_ms: 0,
        }
    }

    /// A chapter with no title.
    ///
    /// Legal and worth keeping: a file that marks boundaries without naming them
    /// still navigates correctly, and inventing "Track 4" would misrepresent
    /// the source.
    #[must_use]
    pub fn untitled(start_ms: u64, part: usize) -> Self {
        Self::new("", start_ms, part)
    }

    /// The index of the first part this chapter occupies.
    #[must_use]
    pub fn first_part(&self) -> usize {
        self.parts.first().copied().unwrap_or(0)
    }

    /// Whether this chapter crosses a part boundary.
    #[must_use]
    pub fn spans_parts(&self) -> bool {
        self.parts.len() > 1
    }

    /// The chapter's whole-title start, in milliseconds.
    ///
    /// Correct only after [`crate::Title::laid_out`] has run, because a container
    /// never states it: the offset depends on the lengths of every preceding part,
    /// which the chapter cannot see. Before that it reads as the part-local time,
    /// which is a plausible wrong answer rather than an obviously absent one — so
    /// titles assembled by this crate always lay out before handing back.
    #[must_use]
    pub fn title_start_ms(&self) -> u64 {
        self.title_offset_ms.saturating_add(self.start_ms)
    }

    /// Set the whole-title offset. Called by [`crate::Title::laid_out`].
    pub(crate) fn set_title_offset(&mut self, offset_ms: u64) {
        self.title_offset_ms = offset_ms;
    }

    /// File this chapter under a single part, discarding its span.
    ///
    /// Used when a chapter's declared parts do not exist, so its span names files
    /// this title does not have. Replacing the span keeps the stored index and the
    /// part holding it in agreement, which every other operation assumes.
    pub(crate) fn file_under(&mut self, part: usize) {
        self.parts = vec![part];
    }

    /// Renumber the parts this chapter refers to through `mapping`.
    ///
    /// `mapping[old] = new`. Called by [`crate::Title::with_part_at`], because a
    /// chapter naming a part by index goes stale the moment the parts are
    /// reordered — and a stale index is worse than no index, since it points at a
    /// real but wrong file.
    pub(crate) fn remap_parts(&mut self, mapping: &[usize]) {
        for p in &mut self.parts {
            if let Some(new) = mapping.get(*p) {
                *p = *new;
            }
        }
        // The parts must stay in playing order, or `first_part` stops meaning
        // "where this chapter begins".
        self.parts.sort_unstable();
    }

    /// The duration to the next chapter, or `None` for the last one.
    ///
    /// Both times must be in the same frame of reference — file-local with
    /// file-local, or title-absolute with title-absolute. Mixing them yields a
    /// plausible wrong number, so [`Chapter::title_start_ms`] exists to make the
    /// absolute form a single call.
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

/// Concatenate per-file chapter lists into one flat list for a title.
///
/// Each chapter keeps its **part-local** time and records which part it came from,
/// because that is what the source states and what a seek within one file needs.
/// The whole-title offsets come from [`crate::Title::from_flat_chapters`], which is
/// the only place that can know them — they depend on the parts' lengths, not on the
/// chapters.
///
/// # Why the per-file lengths are not a parameter
///
/// They are not needed to flatten the chapters, and they are not enough on their
/// own: a whole-title offset needs the parts assembled. Accepting them here and not
/// using them would make a signature lie about where the numbers come from. So they
/// are supplied once, to the title:
///
/// ```
/// # use audiobook_core::{Title, chapter::concatenate};
/// # let files: Vec<Vec<(u64, String)>> = vec![vec![(0, "One".into())], vec![(0, "Two".into())]];
/// let title = Title::from_flat_chapters(
///     concatenate(&files),
///     &[600_000, 300_000],
///     "Book",
///     "Author",
/// );
/// assert_eq!(title.chapters()[1].start_ms, 600_000);
/// ```
///
/// # Why no chapter is merged across a boundary
///
/// A chapter that runs to the end of file `i` and continues into file `i + 1` is a
/// real thing, and a chapter list that represents it has to span two parts. This
/// function does **not** merge, because nothing in the inputs says which chapters
/// continue — a file's last chapter ending at its last chapter-start is not evidence
/// of continuation, it is just what an open-ended chapter looks like.
///
/// Guessing would produce exactly the kind of lie this crate exists to avoid: a
/// boundary invented or erased on the way into a library index. Use
/// [`concatenate_spanning`] where the caller *does* know — a sidecar chapter file, an
/// EPUB table of contents, or a producer that said so.
#[must_use]
pub fn concatenate(files: &[Vec<(u64, String)>]) -> Vec<Chapter> {
    let mut out: Vec<Chapter> = Vec::with_capacity(files.iter().map(Vec::len).sum());
    for (i, file) in files.iter().enumerate() {
        for (start, title) in file {
            out.push(Chapter::new(title, *start, i));
        }
    }
    out
}

/// Concatenate per-file chapters, letting the caller declare which chapters
/// continue into the following file.
///
/// As with [`concatenate`], no per-file lengths are taken here: see that function
/// for why they belong to the title.
///
/// `continues[i]`, when present and true, means **file `i`'s last chapter runs into
/// file `i + 1`**. That single bit per file is all the information the merge needs,
/// and requiring the caller to state it is what keeps the result honest: the
/// alternative — inferring continuation from an open-ended final chapter — merges
/// chapters that were never one chapter.
///
/// # Where a span ends
///
/// A chapter that continues covers file `i` and then every following file, up to the
/// first one whose **first chapter starts at zero**. That is the file where the
/// chapter ends: the next chapter begins at the very first instant of it, so no audio
/// in that file belongs to the chapter that ran into it.
///
/// The rule is derived from the inputs rather than guessed, which matters because
/// guessing either swallows the next chapter or truncates this one. A file whose
/// first chapter starts *after* zero contributes its opening audio to the chapter
/// running into it, so it is included.
///
/// A file with no chapters cannot continue into anything, so a `true` for such a
/// file is ignored rather than creating a chapter.
#[must_use]
pub fn concatenate_spanning(files: &[Vec<(u64, String)>], continues: &[bool]) -> Vec<Chapter> {
    let mut out: Vec<Chapter> = Vec::with_capacity(files.iter().map(Vec::len).sum());

    for (i, file) in files.iter().enumerate() {
        if file.is_empty() {
            continue;
        }
        let spans_onward = continues.get(i).copied().unwrap_or(false) && i + 1 < files.len();

        for (j, (start, title)) in file.iter().enumerate() {
            let is_last = j + 1 == file.len();
            if is_last && spans_onward {
                // The chapter covers this file and every following file up to the
                // first one that declares chapters — those would end it.
                let end = files
                    .iter()
                    .enumerate()
                    .skip(i + 1)
                    .find(|(_, f)| f.first().is_some_and(|(start, _)| *start == 0))
                    .map_or(files.len(), |(k, _)| k);
                let span: Vec<usize> = (i..end).collect();
                out.push(Chapter::spanning(title, *start, &span));
            } else {
                out.push(Chapter::new(title, *start, i));
            }
        }
    }

    // The last declared chapter may have been marked as continuing, but there is
    // nothing after it. Trim its span to the files that actually exist, which for
    // the final chapter means the file it started in.
    if let Some(last) = out.last_mut() {
        let highest = files.len().saturating_sub(1);
        last.parts.retain(|p| *p <= highest);
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
        let a = Chapter::new("One", 0, 0);
        let b = Chapter::new("Two", 1_000, 0);
        assert_eq!(a.duration_ms(b.start_ms), Some(1_000));
        // The last chapter has no known end.
        assert_eq!(b.duration_ms(5_000), Some(4_000));
    }

    #[test]
    fn an_untitled_chapter_is_legal() {
        let c = Chapter::untitled(500, 0);
        assert_eq!(c.title, "");
        assert_eq!(c.start_ms, 500);
    }

    #[test]
    fn concatenation_keeps_chapters_part_local() {
        // Each chapter keeps the time its own container stated and records the part
        // it came from. Nothing here is title-absolute, because nothing in a
        // per-file container knows the other files' lengths — that is
        // `Title::laid_out`'s job, and doing it here would mean guessing.
        let files = vec![
            vec![(0u64, String::from("A1")), (150_000, String::from("A2"))],
            vec![(0u64, String::from("B1")), (90_000, String::from("B2"))],
        ];
        let out = concatenate(&files);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].start_ms, 0);
        assert_eq!(out[1].start_ms, 150_000);
        assert_eq!(out[2].start_ms, 0, "file 2's own container says zero");
        assert_eq!(out[2].parts, alloc::vec![1], "but it belongs to part 1");
        assert_eq!(out[3].start_ms, 90_000);
        assert!(out[..2].iter().all(|c| c.first_part() == 0));
        assert!(out[2..].iter().all(|c| c.first_part() == 1));
    }

    #[test]
    fn a_spanning_chapter_names_every_part_it_covers() {
        // A chapter running to the end of file 0 and on through two files that
        // declare nothing of their own. This is the three-level structure real M4B
        // tooling uses — parts → chapters → files — and a model where a chapter lives
        // in exactly one file cannot represent it at all.
        let files = vec![
            vec![(0u64, String::from("Long"))],
            Vec::new(),
            Vec::new(),
            vec![(0u64, String::from("After"))],
        ];
        let out = concatenate_spanning(&files, &[true, false, false, false]);

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "Long");
        assert_eq!(out[0].parts, alloc::vec![0, 1, 2]);
        assert!(out[0].spans_parts());
        assert_eq!(out[1].title, "After");
        assert_eq!(out[1].parts, alloc::vec![3]);
    }

    #[test]
    fn a_span_ends_where_a_next_file_starts_its_first_chapter_at_zero() {
        // File 1's first chapter is at 0, so the chapter running into file 1 ends at
        // exactly the boundary and none of file 1's audio is its own. Deriving this
        // rather than guessing is what keeps a span from swallowing the next chapter.
        let files = vec![
            vec![(0u64, String::from("Runs On"))],
            vec![(0u64, String::from("Ends Here"))],
        ];
        let out = concatenate_spanning(&files, &[true, false]);
        assert_eq!(
            out[0].parts,
            alloc::vec![0],
            "the boundary file contributes nothing, so it is not in the span"
        );
        assert_eq!(out[0].title, "Runs On");
        assert_eq!(out[1].parts, alloc::vec![1], "the next file keeps its own");
        assert_eq!(out[1].title, "Ends Here");
    }

    #[test]
    fn a_span_includes_a_next_file_that_starts_mid_chapter() {
        // File 1's first chapter is 30 s in, so 30 s of file 1 belongs to the chapter
        // that ran into it. That is the case a naive "stop at the next file with
        // chapters" rule gets wrong.
        let files = vec![
            vec![(0u64, String::from("Straddles"))],
            vec![(30_000, String::from("Next"))],
        ];
        let out = concatenate_spanning(&files, &[true, false]);
        assert_eq!(out[0].parts, alloc::vec![0, 1]);
        assert_eq!(out[1].parts, alloc::vec![1]);
    }

    #[test]
    fn spanning_is_never_inferred_from_an_open_ended_chapter() {
        // Nothing in the inputs says a chapter continues, so nothing is merged.
        // Guessing would invent or erase a boundary, which is the exact failure this
        // crate exists to avoid.
        let files = vec![
            vec![(0u64, String::from("A"))],
            vec![(0u64, String::from("B"))],
        ];
        let out = concatenate(&files);
        assert!(out.iter().all(|c| !c.spans_parts()));
        assert_eq!(out[0].parts, alloc::vec![0]);
        assert_eq!(out[1].parts, alloc::vec![1]);
    }

    #[test]
    fn a_file_with_no_chapters_cannot_continue_into_anything() {
        let files = vec![Vec::new(), vec![(0u64, String::from("B"))]];
        let out = concatenate_spanning(&files, &[true, false]);
        assert_eq!(
            out.len(),
            1,
            "the empty file contributes nothing to merge into"
        );
        assert_eq!(out[0].title, "B");
        assert_eq!(out[0].parts, alloc::vec![1]);
    }

    #[test]
    fn a_span_is_trimmed_to_the_parts_that_exist() {
        // The final chapter marked as continuing has nothing after it, so its span
        // must not name a part that does not exist.
        let files = vec![vec![(0u64, String::from("Only"))]];
        let out = concatenate_spanning(&files, &[true]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].parts, alloc::vec![0]);
        assert!(!out[0].spans_parts());
    }

    #[test]
    fn a_spanning_chapter_with_no_parts_lands_somewhere_real() {
        // Handed an empty span it belongs to part 0 rather than to nothing; dropping
        // it would lose a boundary silently.
        let c = Chapter::spanning("X", 0, &[]);
        assert_eq!(c.parts, alloc::vec![0]);
        assert_eq!(c.first_part(), 0);
    }

    #[test]
    fn a_part_reports_durations_from_its_chapters() {
        let mut p = Part::new("Book One");
        p.chapters.push(Chapter::new("One", 0, 0));
        p.chapters.push(Chapter::new("Two", 60_000, 0));
        p.chapters.push(Chapter::new("Three", 120_000, 0));

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
        p.chapters.push(Chapter::new("One", 0, 0));
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
    use crate::{Part, Title};
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

            // Concatenation preserves what each container stated, and attributes
            // every chapter to the file it came from.
            let out = concatenate(&files);

            let mut expected: Vec<(String, u64, usize)> = Vec::new();
            for (i, file) in files.iter().enumerate() {
                for (start, title) in file {
                    expected.push((title.clone(), *start, i));
                }
            }
            let actual: Vec<(String, u64, usize)> = out
                .iter()
                .map(|c| (c.title.clone(), c.start_ms, c.first_part()))
                .collect();
            prop_assert_eq!(actual, expected);

            // Laying the same chapters out into parts is what turns the local times
            // into whole-title times, and that is where the durations are used.
            let mut title = Title::new("Book", "Author");
            for (i, file) in files.iter().enumerate() {
                title = title.with_part(Part {
                    title: String::new(),
                    chapters: file
                        .iter()
                        .map(|(start, t)| Chapter::new(t, *start, i))
                        .collect(),
                    offset_ms: 0,
                    duration_ms: durs.get(i).copied(),
                });
            }
            let title = title.laid_out();

            let mut expected_absolute: Vec<(String, u64)> = Vec::new();
            let mut offset = 0u64;
            for (i, file) in files.iter().enumerate() {
                for (start, title) in file {
                    expected_absolute.push((title.clone(), offset + start));
                }
                offset += durs[i];
            }
            let actual_absolute: Vec<(String, u64)> = title
                .chapters()
                .iter()
                .map(|c| (c.title.clone(), c.start_ms))
                .collect();
            prop_assert_eq!(actual_absolute, expected_absolute);
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
            let out = concatenate(&files);
            let names: Vec<&str> = out.iter().map(|c| c.title.as_str()).collect();
            let flattened: Vec<&str> = files.iter().flatten().map(|(_, t)| t.as_str()).collect();
            prop_assert_eq!(names, flattened);

            // Every chapter's part index equals the file it came from, which is the
            // invariant `Title::laid_out` relies on.
            let mut expected: Vec<usize> = Vec::new();
            for (expected_part, file) in files.iter().enumerate() {
                expected.extend(core::iter::repeat_n(expected_part, file.len()));
            }
            let actual: Vec<usize> = out.iter().map(|c| c.first_part()).collect();
            prop_assert_eq!(actual, expected);

            // Concatenation never invents a span.
            prop_assert!(out.iter().all(|c| !c.spans_parts()));
        }
    }
}
