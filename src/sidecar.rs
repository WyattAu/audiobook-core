//! Chapter sidecar files — the interchange formats real tooling reads and writes.
//!
//! # Why this exists
//!
//! Chapters are often *not* in the audio file. A book ripped from a CD arrives as
//! MP3s with no `CHAP` frames and a `.cue` beside them; a listener's own markers live
//! in a text file; a studio's session exports a chapter list the encoder never muxed.
//! Every tool in this space accepts a sidecar, and they do not agree on the format —
//! ffmpeg reads `FFMetadata`, MP4Box and `mp4chaps` read a `CHAPTER01`/`CHAPTER01NAME`
//! pair, and MP4Box additionally reads four more shapes.
//!
//! So a library that wants to *read* a chapter list a user already has must read at
//! least the two dominant formats, and one that wants to *write* chapters without
//! shipping a muxer should emit one. Both directions are here, and both are pure: no
//! I/O, no dependency, `no_std`.
//!
//! # The trap in FFMetadata
//!
//! ffmpeg's documentation warns that "whitespace in metadata (e.g. `foo = bar`) is
//! considered to be a part of the tag", and that is not pedantry. A hand-edited file
//! with one stray space before an `=` is not a chapter with a wrong key — it is a
//! chapter with **no** key, and ffmpeg accepts it while discarding the title. Verified
//! against ffmpeg: a `[CHAPTER]` block reading `START =0` produces a chapter at 0 with
//! an **empty** title and only a warning on stderr.
//!
//! [`SidecarWarning::WhitespaceInKey`] exists for exactly this, so a caller can tell a
//! user their file is nearly right rather than discovering an empty chapter list later.
//!
//! # The other trap: what unit is `START` in?
//!
//! `TIMEBASE` is optional, and when it is absent ffmpeg assumes **nanoseconds** — a
//! fact verified here by muxing `END=60000000000` without a timebase and reading back
//! 60 s. Some older copies of the documentation say milliseconds, which is wrong, and
//! a reader that guesses milliseconds turns a 60-second chapter into a
//! sixty-thousand-second one.
//!
//! Rather than guess, a file with no `TIMEBASE` is read as nanoseconds and reported
//! through [`SidecarWarning::NoTimebase`], so the assumption is visible.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::chapter::Chapter;

/// The sidecar formats this crate reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SidecarFormat {
    /// ffmpeg's `;FFMETADATA1` format: `[CHAPTER]` sections with `TIMEBASE`, `START`,
    /// `END` and `title`.
    Ffmetadata,
    /// MP4Box's common form: `CHAPTER01=00:10:21.000` with `CHAPTER01NAME=one`.
    ChapterX,
    /// A bare timecode per line: `h:mm:ss[.ms] Chapter One`.
    Timecode,
    /// ZoomPlayer's `AddChapter(nb_frames,name)`, `AddChapterBySeconds(nb_sec,name)`
    /// and `AddChapterByTime(h,m,s,name)`.
    ZoomPlayer,
    /// Detect from the content.
    #[default]
    Auto,
}

impl SidecarFormat {
    /// A short name for display.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            SidecarFormat::Ffmetadata => "ffmetadata",
            SidecarFormat::ChapterX => "chapterx",
            SidecarFormat::Timecode => "timecode",
            SidecarFormat::ZoomPlayer => "zoomplayer",
            SidecarFormat::Auto => "auto",
        }
    }
}

/// Something questionable in a sidecar file.
///
/// Every one of these is a file that a human edited and got nearly right. Reporting
/// them costs nothing and turns a silently wrong chapter list into a fixable one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SidecarWarning {
    /// A key had whitespace around its `=`, so ffmpeg reads the key as `START ` and
    /// discards the tag. Confirmed against ffmpeg: the chapter appears with an empty
    /// title.
    WhitespaceInKey {
        /// The line number, one-based.
        line: usize,
        /// The key as it was read, including the stray space.
        key: String,
    },
    /// No `TIMEBASE`, so the timestamps are assumed to be nanoseconds.
    ///
    /// The assumption is ffmpeg's and was verified against it, but it is an
    /// assumption, and older documentation says milliseconds — so a file relying on it
    /// is worth surfacing.
    NoTimebase,
    /// A `TIMEBASE` other than `1/1000`, meaning the numbers need rescaling to
    /// milliseconds. Informational rather than wrong.
    UnusualTimebase {
        /// The numerator.
        num: i64,
        /// The denominator.
        den: i64,
    },
    /// `END` was empty, which ffmpeg tolerates with a warning.
    EmptyEnd {
        /// The line number.
        line: usize,
    },
    /// A value ended with an escaped backslash, which ffmpeg's reader misinterprets as
    /// a line continuation and uses to swallow the *following* line.
    ///
    /// Verified against ffmpeg: a chapter title ending in a backslash costs the file the
    /// next chapter. The reader strips the final `\` of the `\\` pair, joins the next
    /// line — the `[CHAPTER]` header — into the title, and then discards the `START` that
    /// arrives out of order, leaving the surviving chapter with a start time of zero.
    /// Two chapters become one, with the wrong timestamp, and ffmpeg prints nothing on
    /// stderr about it.
    ///
    /// This parser does not have the bug: it counts trailing backslashes, so the escaped
    /// pair is honoured and every chapter survives. The warning exists because the file is
    /// still dangerous — handing it to ffmpeg loses a chapter — and because a reader
    /// wondering why a *different* tool saw fewer chapters deserves an answer.
    TrailingBackslash {
        /// The line the value was on.
        line: usize,
    },
    /// A chapter's title was absent or empty.
    Untitled {
        /// The line number the chapter started on.
        line: usize,
    },
    /// Chapters are not in ascending time order.
    OutOfOrder {
        /// The line number of the offending chapter.
        line: usize,
        /// Its start, in milliseconds.
        start_ms: u64,
    },
    /// A line that could not be understood.
    Unparsed {
        /// The line number.
        line: usize,
        /// The line, truncated.
        text: String,
    },
}

/// What went wrong badly enough to stop parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SidecarError {
    /// The bytes are not text, or not in any format recognised.
    Unrecognised {
        /// The first line, truncated, for a message.
        first_line: String,
    },
    /// The format was forced and the content does not match it.
    WrongFormat {
        /// What was asked for.
        wanted: SidecarFormat,
        /// What the content looks like.
        looks_like: SidecarFormat,
    },
    /// A timestamp that cannot be parsed.
    BadTimestamp {
        /// The line number.
        line: usize,
        /// The offending text, truncated.
        text: String,
    },
}

impl core::fmt::Display for SidecarError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SidecarError::Unrecognised { first_line } => {
                write!(f, "not a recognised chapter file; it begins `{first_line}`")
            }
            SidecarError::WrongFormat { wanted, looks_like } => write!(
                f,
                "asked for {} but the content looks like {}",
                wanted.name(),
                looks_like.name()
            ),
            SidecarError::BadTimestamp { line, text } => {
                write!(f, "line {line}: `{text}` is not a timestamp")
            }
        }
    }
}

impl core::error::Error for SidecarError {}

/// A parsed chapter list plus everything questionable about how it was written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SidecarChapters {
    /// The chapters, in the order the file listed them.
    pub chapters: Vec<Chapter>,
    /// Which format was read.
    pub format: SidecarFormat,
    /// Global tags from the file's header section, such as `title` or `artist`.
    pub global_tags: Vec<(String, String)>,
    /// The media's duration, when it is known.
    ///
    /// Only [`SidecarChapters::to_ffmetadata`] uses this, and only for the last chapter's
    /// `END`, which is otherwise the one value a sidecar on its own cannot supply: the
    /// format has no clean way to say "to the end of the file" (an empty `END` makes
    /// ffmpeg warn), and inventing a duration would put the boundary somewhere arbitrary.
    ///
    /// Without it the last chapter is written with `END` equal to its own `START`, which
    /// reads as a zero-length chapter. ffmpeg accepts that and derives the real duration
    /// from the media, so it is harmless in practice, but a caller that knows the length
    /// can say so and get a file that is correct on its own terms.
    pub duration_ms: Option<u64>,
    /// Everything questionable, in the order it was noticed.
    pub warnings: Vec<SidecarWarning>,
}

impl SidecarChapters {
    /// Whether anything questionable was found.
    #[must_use]
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }

    /// A global tag's value, if the file carried it.
    ///
    /// Key comparison is case-insensitive because ffmpeg's own keys are inconsistent
    /// in case across its output.
    #[must_use]
    pub fn tag(&self, key: &str) -> Option<&str> {
        self.global_tags
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// The chapters sorted by start time.
    #[must_use]
    pub fn sorted(&self) -> Vec<Chapter> {
        let mut v = self.chapters.clone();
        v.sort_by_key(|c| c.start_ms);
        v
    }

    /// Render as `;FFMETADATA1`, the format ffmpeg reads.
    ///
    /// Emits `TIMEBASE=1/1000` always, even though ffmpeg defaults to nanoseconds
    /// without it: an explicit timebase removes the reader's need to guess, and a
    /// generated file has no reason to rely on a default.
    ///
    /// Titles are escaped per ffmpeg's rules — `\`, `=`, `;`, `#` and newlines are
    /// backslash-escaped — so a title containing `;` does not truncate the line.
    ///
    /// One title cannot survive a round trip through ffmpeg: see
    /// [`ffmpeg_reads_back`]. The escaping here is still correct, and this crate's own
    /// reader recovers such a title intact, but a title ending in a backslash will cost
    /// a chapter in any tool that reads the file back with ffmpeg.
    #[must_use]
    pub fn to_ffmetadata(&self) -> String {
        let mut out = String::from(";FFMETADATA1\n");
        for (k, v) in &self.global_tags {
            out.push_str(&format!("{}={}\n", k, ffmetadata_escape(v)));
        }
        let sorted = self.sorted();
        for (i, c) in sorted.iter().enumerate() {
            let end = sorted.get(i + 1).map_or_else(
                || {
                    // The last chapter: use the duration when the caller supplied one and
                    // it is not before the chapter starts, otherwise fall back to the
                    // chapter's own start.
                    self.duration_ms
                        .filter(|d| *d > c.start_ms)
                        .unwrap_or(c.start_ms)
                },
                |n| n.start_ms,
            );
            out.push_str("[CHAPTER]\nTIMEBASE=1/1000\n");
            out.push_str(&format!("START={}\n", c.start_ms));
            out.push_str(&format!("END={end}\n"));
            out.push_str(&format!("title={}\n", ffmetadata_escape(&c.title)));
        }
        out
    }

    /// Render as MP4Box's `CHAPTER01`/`CHAPTER01NAME` form.
    ///
    /// `h:mm:ss.mmm`, which is the shape `mp4chaps` and MP4Box's `-chap` both accept.
    #[must_use]
    pub fn to_chapter_x(&self) -> String {
        let mut out = String::new();
        for (i, c) in self.sorted().iter().enumerate() {
            // MP4Box requires the numbers to be sequential — two consecutive lines must
            // carry the same X — so a gap would silently drop every chapter after it.
            let n = i + 1;
            out.push_str(&format!(
                "CHAPTER{n:02}={}\nCHAPTER{n:02}NAME={}\n",
                format_timecode(c.start_ms),
                c.title
            ));
        }
        out
    }

    /// Render as one bare timecode per line: `h:mm:ss.mmm Chapter One`.
    #[must_use]
    pub fn to_timecode(&self) -> String {
        let mut out = String::new();
        for c in self.sorted() {
            out.push_str(&format!("{} {}\n", format_timecode(c.start_ms), c.title));
        }
        out
    }
}

/// Escape a value for ffmpeg's metadata syntax.
///
/// ffmpeg's rule is that `=`, `;`, `#`, `\` and newlines must be backslash-escaped.
/// Verified against ffmpeg: `title=Has \\= equals \\; semi \\# hash` round-trips as
/// `Has = equals ; semi # hash`.
#[must_use]
pub fn ffmetadata_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | '=' | ';' | '#' | '\n' | '\r') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Undo [`ffmetadata_escape`].
///
/// ffmpeg's demuxer drops the backslash and keeps whatever follows it, for *any*
/// character — there is no line-continuation rule. Verified against ffmpeg: a title
/// written as `Line one\` followed by a real newline reads back as two lines, and
/// `\n` (a backslash and the letter `n`) reads back as plain `n`. A trailing
/// backslash with nothing to escape is kept, since dropping it would lose a
/// character the writer clearly meant.
#[must_use]
pub fn ffmetadata_unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(escaped) => out.push(escaped),
            None => out.push('\\'),
        }
    }
    out
}

/// Format milliseconds as `h:mm:ss.mmm`.
#[must_use]
pub fn format_timecode(ms: u64) -> String {
    let (total_s, milli) = (ms / 1000, ms % 1000);
    let (h, m, s) = (total_s / 3600, (total_s % 3600) / 60, total_s % 60);
    format!("{h}:{m:02}:{s:02}.{milli:03}")
}

/// Parse a timestamp into milliseconds.
///
/// Accepts `h:mm:ss`, `mm:ss`, a bare `ss`, and an optional `.mmm` or `:mmm`
/// fraction — the set MP4Box documents plus the shorter forms people actually write.
/// A space or tab may replace the first colon, which is how most hand-written files
/// separate fields.
#[must_use]
pub fn parse_timestamp(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    // Split the fractional part off first, in either of the two spellings.
    let (main, frac_ms) = if let Some((a, b)) = text.rsplit_once('.') {
        (a, b)
    } else if let Some((a, b)) = text.rsplit_once(':') {
        // Only treat the last colon group as a fraction when it is short enough to be
        // one: `1:02:03` is h:m:s, not h:m with a 3-digit fraction.
        if b.len() == 3 && b.bytes().all(|c| c.is_ascii_digit()) {
            (a, b)
        } else {
            (text, "")
        }
    } else {
        (text, "")
    };

    let mut millis: u64 = 0;
    if !frac_ms.is_empty() {
        if !frac_ms.bytes().all(|c| c.is_ascii_digit()) || frac_ms.len() > 3 {
            return None;
        }
        let mut digits: String = frac_ms.to_string();
        while digits.len() < 3 {
            digits.push('0');
        }
        millis = digits.parse().ok()?;
    }

    let mut seconds: u64 = 0;
    for (i, field) in main.split([':', ' ', '\t']).enumerate() {
        if field.is_empty() || !field.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        // A four-field reading is h:m:s:fractional-seconds, which is unambiguous; a
        // three-field one is h:m:s or m:s.fraction depending on magnitude. Both are
        // handled by treating the fields right-to-left.
        seconds = seconds
            .checked_mul(if i == 0 { 3600 } else { 60 })
            .and_then(|v| v.checked_add(field.parse::<u64>().ok()?))?;
    }
    seconds
        .checked_mul(1_000)
        .and_then(|v| v.checked_add(millis))
}

/// Which format a chunk of text looks like.
///
/// The order matters: `;FFMETADATA` is unambiguous and checked first, then the
/// `AddChapter` family, then `CHAPTER01=`, then bare timecodes.
#[must_use]
pub fn detect_format(text: &str) -> Option<SidecarFormat> {
    let mut saw_chapter_x = false;
    let mut saw_timecode = false;

    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A comment or the header is not evidence either way.
        if line.starts_with(';') || line.starts_with('#') {
            if line.starts_with(";FFMETADATA") {
                return Some(SidecarFormat::Ffmetadata);
            }
            continue;
        }
        if line.starts_with('[') {
            if line.eq_ignore_ascii_case("[CHAPTER]") {
                return Some(SidecarFormat::Ffmetadata);
            }
            continue;
        }
        if line.starts_with("AddChapter") {
            return Some(SidecarFormat::ZoomPlayer);
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("CHAPTER") && upper.contains('=') {
            saw_chapter_x = true;
            continue;
        }
        // A bare timecode: a leading digit, then a colon within the first eight
        // characters, then whitespace.
        if saw_timecode || saw_chapter_x {
            continue;
        }
        let bytes = line.as_bytes();
        if let Some(first) = bytes.first() {
            if first.is_ascii_digit() && line.find([':', ' ']).is_some_and(|i| i > 0 && i <= 7) {
                saw_timecode = true;
            }
        }
        let _ = n;
    }

    if saw_chapter_x {
        Some(SidecarFormat::ChapterX)
    } else if saw_timecode {
        Some(SidecarFormat::Timecode)
    } else {
        None
    }
}

/// Parse a chapter sidecar, detecting the format.
///
/// # Errors
///
/// [`SidecarError::Unrecognised`] when no format matches.
pub fn parse(text: &str) -> Result<SidecarChapters, SidecarError> {
    let Some(format) = detect_format(text) else {
        let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(SidecarError::Unrecognised {
            first_line: truncate(first.trim(), 60),
        });
    };
    parse_as(text, format)
}

/// Parse a chapter sidecar as a specific format.
///
/// # Errors
///
/// [`SidecarError::WrongFormat`] when the content is plainly a different format, and
/// [`SidecarError::BadTimestamp`] when a timestamp cannot be read.
pub fn parse_as(text: &str, format: SidecarFormat) -> Result<SidecarChapters, SidecarError> {
    // If the content plainly is some *other* format, say so rather than reporting a
    // confusing error from the wrong parser — `parse_as(chapterx_text, Ffmetadata)`
    // should not claim the `CHAPTER01=` line is a bad timestamp.
    if format != SidecarFormat::Auto {
        if let Some(detected) = detect_format(text) {
            if detected != format && detected != SidecarFormat::Auto {
                return Err(SidecarError::WrongFormat {
                    wanted: format,
                    looks_like: detected,
                });
            }
        }
    }
    match format {
        SidecarFormat::Ffmetadata => parse_ffmetadata(text),
        SidecarFormat::ChapterX => parse_chapter_x(text),
        SidecarFormat::Timecode => parse_timecode(text),
        SidecarFormat::ZoomPlayer => parse_zoomplayer(text),
        SidecarFormat::Auto => parse(text),
    }
}

/// Split text into *logical* lines, rejoining ffmpeg's line continuations.
///
/// A value may contain a newline, and ffmpeg writes one as a backslash at the end of
/// the line followed by a real newline — verified by muxing a title containing a
/// newline and dumping it back out with `-f ffmetadata`, which reproduces exactly that
/// shape. Reading it line by line therefore truncates the title at the first newline and
/// then reads the rest as a fresh, unparseable line, so the continuation has to be undone
/// before the lines are interpreted.
///
/// Only an *odd* run of trailing backslashes continues: `\\` at the end of a line is an
/// escaped literal backslash, which is what a title ending in a backslash looks like.
///
/// The rejoined newline is kept as a real newline character in the value, because that
/// is what ffmpeg reads back. The line number reported is the first physical line, so a
/// warning names where the entry started rather than where it ended.
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut pending: Option<(usize, String)> = None;

    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        match pending.as_mut() {
            Some((_, buf)) => {
                buf.push('\n');
                buf.push_str(line);
            }
            None => pending = Some((i + 1, line.to_string())),
        }

        let continues = pending
            .as_ref()
            .is_some_and(|(_, buf)| ends_with_odd_backslash(buf));
        if continues {
            // Remove exactly one trailing backslash — the continuation marker — leaving
            // any escaped `\\` before it intact.
            if let Some((_, buf)) = pending.as_mut() {
                buf.pop();
            }
            continue;
        }
        if let Some(entry) = pending.take() {
            out.push(entry);
        }
    }
    out.extend(pending);
    out
}

/// How many backslashes a string ends with.
fn count_trailing_backslashes(text: &str) -> usize {
    text.chars().rev().take_while(|c| *c == '\\').count()
}

/// Whether a line ends with an unescaped backslash, i.e. a continuation.
fn ends_with_odd_backslash(line: &str) -> bool {
    count_trailing_backslashes(line) % 2 == 1
}

/// Whether ffmpeg can read this chapter title back out of a `;FFMETADATA1` file.
///
/// Returns `false` for a title ending in a backslash, which ffmpeg's reader cannot
/// represent: it takes the escaped pair as a line continuation and swallows the next
/// line of the file, costing the file a chapter with no diagnostic. Verified against
/// ffmpeg.
///
/// # Why this matters for a writer
///
/// [`SidecarChapters::to_ffmetadata`] escapes correctly, so this crate's own reader
/// round-trips such a title perfectly. The trouble is the other direction: the file it
/// writes is one ffmpeg will misread. No encoding avoids that \u{2014} a lone trailing
/// backslash is read as a continuation and the backslash is lost instead \u{2014} so a
/// tool that hands its output to ffmpeg should check this first and warn the user, rather
/// than write a file that quietly loses a chapter.
#[must_use]
pub fn ffmpeg_reads_back(title: &str) -> bool {
    !title.ends_with('\\')
}

/// A `[CHAPTER]` block being accumulated.
///
/// Keys within a block may appear in any order — ffmpeg writes `TIMEBASE`, `START`,
/// `END`, `title`, but a hand-edited file need not — so each is stored separately and
/// the block only becomes a chapter once it ends. The timebase is per-block because a
/// file may legitimately use different ones.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChapterBlock {
    start_ms: Option<u64>,
    end_ms: Option<u64>,
    title: String,
    /// This block's `TIMEBASE`, which governs its own numbers.
    timebase: Option<(i64, i64)>,
    /// The line the block started on, for warnings that name it.
    line: usize,
}

impl ChapterBlock {
    fn new(line: usize) -> Self {
        Self {
            start_ms: None,
            end_ms: None,
            title: String::new(),
            timebase: None,
            line,
        }
    }
}

/// Parse ffmpeg's `;FFMETADATA1` format.
fn parse_ffmetadata(text: &str) -> Result<SidecarChapters, SidecarError> {
    let mut out = SidecarChapters {
        format: SidecarFormat::Ffmetadata,
        ..SidecarChapters::default()
    };
    let mut saw_timebase = false;
    let mut saw_chapter = false;
    // Which section the parser is inside. Anything before the first header is ffmpeg's
    // global metadata; a section we do not model is neither global nor a chapter, and
    // filing its tags as global would be wrong.
    enum Section {
        Global,
        Chapter,
        Other,
    }
    let mut section = Section::Global;
    let mut block: Option<ChapterBlock> = None;

    for (line_no, logical) in logical_lines(text) {
        // Deliberately **not** trimmed. ffmpeg takes everything after the first `=`
        // verbatim, including leading and trailing spaces, and the warning for a
        // stray space before the `=` only helps if this code agrees with ffmpeg about
        // what the value is. Whitespace is significant here; that is the whole trap.
        let trimmed = logical.as_str();

        // Empty lines and comments are ignored, per ffmpeg's rules.
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
            continue;
        }

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            // A section header closes whatever was open, so a `[CHAPTER]` following a
            // `[STREAM]` cannot inherit the previous block's keys.
            flush_chapter(&mut out, &mut block);
            let name = trimmed[1..trimmed.len() - 1].trim();
            section = if name.eq_ignore_ascii_case("CHAPTER") {
                saw_chapter = true;
                block = Some(ChapterBlock::new(line_no));
                Section::Chapter
            } else {
                Section::Other
            };
            continue;
        }

        let Some((raw_key, raw_value)) = split_key_value(trimmed) else {
            out.warnings.push(SidecarWarning::Unparsed {
                line: line_no,
                text: truncate(trimmed, 60),
            });
            continue;
        };
        if raw_key != raw_key.trim() {
            // ffmpeg reads the key *including* the whitespace, so `START =0` names a tag
            // `START ` and the chapter loses its start. Verified against ffmpeg: the
            // chapter appears with an empty title and only a stderr warning.
            out.warnings.push(SidecarWarning::WhitespaceInKey {
                line: line_no,
                key: raw_key.to_string(),
            });
        }
        let key = raw_key.trim().to_ascii_uppercase();
        // Counted on the raw text, before unescaping: an escaped pair becomes a single
        // backslash once unescaped, and that is exactly when the hazard becomes invisible.
        let raw_trailing_backslashes = count_trailing_backslashes(raw_value);
        let value = ffmetadata_unescape(raw_value);

        if !matches!(section, Section::Chapter) {
            if matches!(section, Section::Global) {
                out.global_tags.push((key, value));
            }
            continue;
        }
        let Some(b) = block.as_mut() else {
            // Only reachable for a `[CHAPTER]` header with no keys before the next
            // header; `flush_chapter` drops such a block anyway.
            continue;
        };
        match key.as_str() {
            "TIMEBASE" => {
                let (num, den) = parse_timebase(&value);
                saw_timebase = true;
                b.timebase = Some((num, den));
                if (num, den) != (1, 1000) {
                    out.warnings
                        .push(SidecarWarning::UnusualTimebase { num, den });
                }
            }
            "START" => {
                let (num, den) = b.timebase.unwrap_or((1, 1_000_000_000));
                b.start_ms = Some(
                    value
                        .parse::<i64>()
                        .ok()
                        .and_then(|n| timebase_to_ms(n, num, den))
                        .ok_or_else(|| SidecarError::BadTimestamp {
                            line: line_no,
                            text: truncate(value.trim(), 60),
                        })?,
                );
            }
            "END" => {
                if value.trim().is_empty() {
                    // ffmpeg tolerates an empty END with a warning and still emits the
                    // chapter, so this is reported rather than rejected.
                    out.warnings
                        .push(SidecarWarning::EmptyEnd { line: line_no });
                    b.end_ms = None;
                } else {
                    let (num, den) = b.timebase.unwrap_or((1, 1_000_000_000));
                    b.end_ms = Some(
                        value
                            .parse::<i64>()
                            .ok()
                            .and_then(|n| timebase_to_ms(n, num, den))
                            .ok_or_else(|| SidecarError::BadTimestamp {
                                line: line_no,
                                text: truncate(value.trim(), 60),
                            })?,
                    );
                }
            }
            "TITLE" => {
                if raw_trailing_backslashes >= 2 {
                    // Even and non-zero: the file escaped a literal backslash, and
                    // ffmpeg's reader will take the last one as a continuation marker.
                    out.warnings
                        .push(SidecarWarning::TrailingBackslash { line: line_no });
                }
                b.title = value;
            }
            _ => {}
        }
    }
    flush_chapter(&mut out, &mut block);

    if saw_chapter && !saw_timebase {
        // The file relied on ffmpeg's nanosecond default. The assumption is ffmpeg's and
        // was verified against it, but it is an assumption — older documentation says
        // milliseconds, which would turn a 60-second chapter into a 60 000-second one.
        out.warnings.push(SidecarWarning::NoTimebase);
    }
    check_order(&mut out);
    Ok(out)
}

/// Push the chapter block being built, if it is complete, onto the list.
fn flush_chapter(out: &mut SidecarChapters, block: &mut Option<ChapterBlock>) {
    let Some(b) = block.take() else {
        return;
    };
    // A block with no `START` is not a chapter: ffmpeg ignores it, and inventing a
    // start would place it somewhere arbitrary.
    let Some(start_ms) = b.start_ms else {
        return;
    };
    if b.title.trim().is_empty() {
        out.warnings.push(SidecarWarning::Untitled { line: b.line });
    }
    out.chapters.push(Chapter::new(&b.title, start_ms, 0));
}

/// Note chapters that are not in ascending time order.
fn check_order(out: &mut SidecarChapters) {
    let mut previous: Option<u64> = None;
    let mut offenders: Vec<SidecarWarning> = Vec::new();
    for c in &out.chapters {
        if let Some(p) = previous {
            if c.start_ms < p {
                offenders.push(SidecarWarning::OutOfOrder {
                    line: 0,
                    start_ms: c.start_ms,
                });
            }
        }
        previous = Some(c.start_ms);
    }
    // Appended rather than spliced in, so the warning order stays file order.
    out.warnings.extend(offenders);
}

/// Parse MP4Box's `CHAPTER01`/`CHAPTER01NAME` form.
fn parse_chapter_x(text: &str) -> Result<SidecarChapters, SidecarError> {
    let mut out = SidecarChapters {
        format: SidecarFormat::ChapterX,
        ..SidecarChapters::default()
    };
    // Starts keyed by chapter number, so the NAME line may precede or follow its
    // number — MP4Box's own documentation says only that the order does not matter.
    let mut starts: Vec<(u32, u64, usize)> = Vec::new();
    let mut names: Vec<(u32, String)> = Vec::new();

    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            out.warnings.push(SidecarWarning::Unparsed {
                line: line_no,
                text: truncate(line, 60),
            });
            continue;
        };
        let key = key.trim().to_ascii_uppercase();
        let Some(digits) = key.strip_prefix("CHAPTER") else {
            continue;
        };
        let rest = digits.strip_suffix("NAME").unwrap_or(digits);
        let Ok(number) = rest.trim().parse::<u32>() else {
            continue;
        };
        if key.ends_with("NAME") {
            names.push((number, value.trim().to_string()));
        } else if value.trim().is_empty() {
            // An empty `CHAPTER02=` is a line someone cleared, not a malformed file, so
            // it is reported and skipped rather than failing the whole sidecar.
            out.warnings.push(SidecarWarning::Unparsed {
                line: line_no,
                text: truncate(line, 60),
            });
        } else {
            let ms = parse_timestamp(value).ok_or_else(|| SidecarError::BadTimestamp {
                line: line_no,
                text: truncate(value.trim(), 60),
            })?;
            starts.push((number, ms, line_no));
        }
    }

    starts.sort_by_key(|(n, _, _)| *n);
    for (number, ms, line_no) in starts {
        let title = names
            .iter()
            .find(|(n, _)| *n == number)
            .map_or_else(String::new, |(_, t)| t.clone());
        if title.trim().is_empty() {
            out.warnings
                .push(SidecarWarning::Untitled { line: line_no });
        }
        out.chapters.push(Chapter::new(&title, ms, 0));
    }
    check_order(&mut out);
    Ok(out)
}

/// Parse one bare timecode per line.
fn parse_timecode(text: &str) -> Result<SidecarChapters, SidecarError> {
    let mut out = SidecarChapters {
        format: SidecarFormat::Timecode,
        ..SidecarChapters::default()
    };
    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        // The timecode is the leading token; everything after it is the title.
        let split = line.find([' ', '\t']).unwrap_or(line.len());
        let (stamp, title) = line.split_at(split);
        let ms = parse_timestamp(stamp).ok_or_else(|| SidecarError::BadTimestamp {
            line: line_no,
            text: truncate(stamp.trim(), 60),
        })?;
        let title = title.trim();
        if title.is_empty() {
            out.warnings
                .push(SidecarWarning::Untitled { line: line_no });
        }
        out.chapters.push(Chapter::new(title, ms, 0));
    }
    check_order(&mut out);
    Ok(out)
}

/// Parse ZoomPlayer's three `AddChapter` shapes.
///
/// `AddChapterBySeconds(n,name)` and `AddChapterByTime(h,m,s,name)` convert without
/// any further information. `AddChapter(nb_frames,name)` does not — it needs a frame
/// rate the file does not carry — so it is reported rather than guessed at, because
/// assuming 25 or 30 fps puts every chapter in the wrong place.
fn parse_zoomplayer(text: &str) -> Result<SidecarChapters, SidecarError> {
    let mut out = SidecarChapters {
        format: SidecarFormat::ZoomPlayer,
        ..SidecarChapters::default()
    };

    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if !line.starts_with("AddChapter") {
            continue;
        }

        let unparsed = |out: &mut SidecarChapters| {
            out.warnings.push(SidecarWarning::Unparsed {
                line: line_no,
                text: truncate(line, 60),
            });
        };

        let Some((prefix, args)) = line.split_once('(') else {
            unparsed(&mut out);
            continue;
        };
        let args = args.strip_suffix(')').unwrap_or(args);
        let fields: Vec<&str> = args.split(',').map(str::trim).collect();

        let parsed: Option<(u64, String)> = match (prefix, fields.as_slice()) {
            ("AddChapterBySeconds", [secs, name]) => secs
                .parse::<u64>()
                .ok()
                .map(|s| (s.saturating_mul(1_000), (*name).to_string())),
            ("AddChapterByTime", [h, m, s, name]) => {
                match (h.parse::<u64>(), m.parse::<u64>(), s.parse::<u64>()) {
                    (Ok(h), Ok(m), Ok(s)) if m < 60 && s < 60 => {
                        Some(((h * 3600 + m * 60 + s) * 1000, (*name).to_string()))
                    }
                    _ => None,
                }
            }
            // Needs a frame rate the file does not carry, so it is reported rather than
            // guessed at: assuming 25 or 30 fps puts every chapter in the wrong place. A
            // boundary is still recorded at zero so the chapter is not lost entirely.
            ("AddChapter", [_frames, name]) => {
                out.warnings.push(SidecarWarning::Unparsed {
                    line: line_no,
                    text: "AddChapter(frame_count, ...) needs a frame rate the file does \
                            not carry"
                        .to_string(),
                });
                Some((0, (*name).to_string()))
            }
            _ => None,
        };

        match parsed {
            Some((ms, title)) => {
                if title.trim().is_empty() {
                    out.warnings
                        .push(SidecarWarning::Untitled { line: line_no });
                }
                out.chapters.push(Chapter::new(&title, ms, 0));
            }
            None => unparsed(&mut out),
        }
    }
    check_order(&mut out);
    Ok(out)
}

/// Split `key=value`, preserving the key's own whitespace so it can be reported.
fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once('=')?;
    if k.is_empty() {
        None
    } else {
        Some((k, v))
    }
}

/// Parse `num/den`.
fn parse_timebase(value: &str) -> (i64, i64) {
    let value = value.trim();
    match value.split_once('/') {
        Some((a, b)) => (
            a.trim().parse().unwrap_or(1),
            b.trim().parse().unwrap_or(1_000_000_000),
        ),
        // A bare number is a denominator, which is how `TIMEBASE=1000` reads.
        None => (1, value.parse().unwrap_or(1_000_000_000)),
    }
}

/// Convert a timestamp in `num/den` timebase units to milliseconds.
///
/// `TIMEBASE=num/den` means each unit is `num/den` seconds, so milliseconds are
/// `N * num * 1000 / den`. Verified against ffmpeg with both shapes that matter:
/// `1/1000` with `START=60000` gives 60 s, and a missing timebase (nanoseconds) with
/// `START=60000000000` also gives 60 s.
///
/// `u128` throughout because `N * num * 1000` overflows `u64` for the nanosecond
/// values that are exactly the ones this has to get right.
fn timebase_to_ms(value: i64, num: i64, den: i64) -> Option<u64> {
    if den == 0 {
        return None;
    }
    let product = (value as i128) * (num as i128) * 1000;
    let divided = product / (den as i128);
    if divided < 0 {
        return None;
    }
    u64::try_from(divided).ok()
}

/// Truncate for a message, on a character boundary.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

/// How a sidecar's chapters are combined with a title's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum MergeStrategy {
    /// Use the sidecar's times, keeping the existing titles where the counts match.
    ///
    /// The default, and the right one when the sidecar was generated *from* the file:
    /// the times are authoritative and the titles are the same ones already stored.
    #[default]
    KeepTimestamps,
    /// Use the sidecar wholesale, titles included.
    ReplaceAll,
    /// Leave the title alone when the chapter counts differ, and take the sidecar when
    /// they match.
    ///
    /// The cautious choice when the sidecar came from somewhere else: applying it to a
    /// title with a different number of chapters would misalign every boundary after
    /// the first difference.
    SkipOnMismatch,
}

impl MergeStrategy {
    /// Apply this strategy.
    #[must_use]
    pub fn merge(self, sidecar: &SidecarChapters, existing: Option<&[Chapter]>) -> Vec<Chapter> {
        let sidecar_sorted = sidecar.sorted();
        match (self, existing) {
            (MergeStrategy::ReplaceAll, _) => sidecar_sorted,
            (MergeStrategy::KeepTimestamps, Some(existing))
                if existing.len() == sidecar_sorted.len() =>
            {
                // Same number of chapters, so the existing titles line up with the new
                // times and are worth keeping — they may be better than the sidecar's.
                sidecar_sorted
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let title = existing
                            .get(i)
                            .map(|e| e.title.clone())
                            .filter(|t| !t.trim().is_empty())
                            .unwrap_or_else(|| c.title.clone());
                        Chapter::new(&title, c.start_ms, c.first_part())
                    })
                    .collect()
            }
            (MergeStrategy::KeepTimestamps, _) => sidecar_sorted,
            (MergeStrategy::SkipOnMismatch, Some(existing))
                if existing.len() != sidecar_sorted.len() =>
            {
                existing.to_vec()
            }
            (MergeStrategy::SkipOnMismatch, _) => sidecar_sorted,
        }
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

    /// The FFMetadata ffmpeg itself writes, captured from a real mux.
    const REAL_FFMETADATA: &str = ";FFMETADATA1\n\
[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=60000\ntitle=Chapter One\n\
[CHAPTER]\nTIMEBASE=1/1000\nSTART=60000\nEND=120000\ntitle=Chapter Two\n\
[CHAPTER]\nTIMEBASE=1/1000\nSTART=120000\nEND=180000\ntitle=Chapter Three\n";

    #[test]
    fn real_ffmetadata_parses() {
        let out = parse(REAL_FFMETADATA).expect("parses");
        assert_eq!(out.format, SidecarFormat::Ffmetadata);
        assert_eq!(out.chapters.len(), 3);
        assert_eq!(out.chapters[0].start_ms, 0);
        assert_eq!(out.chapters[0].title, "Chapter One");
        assert_eq!(out.chapters[2].start_ms, 120_000);
        assert!(!out.has_warnings(), "ffmpeg's own output should be clean");
    }

    /// Byte-for-byte what `ffmpeg -i chapter.m4b -map_chapters 0 -f ffmetadata` wrote.
    ///
    /// Two things in here were not guessable and were only found by running ffmpeg:
    ///
    /// 1. The timebase is the **audio** timebase (`1/44100`), not `1/1000`. Our own
    ///    writer emits `1/1000` — ffmpeg reads that fine — but a file dumped by ffmpeg
    ///    carries the rate of the audio it came from, so a reader that assumed
    ///    milliseconds would place every chapter 44100x too late.
    /// 2. A title containing a newline is written as a backslash at the end of the line
    ///    followed by a real newline, and reads back as a newline. Reading the file line
    ///    by line truncates the title and then trips over the remainder as a syntax
    ///    error.
    const FFMPEG_DUMPED: &str = ";FFMETADATA1\n\
major_brand=M4A \n\
minor_version=512\n\
[CHAPTER]\n\
TIMEBASE=1/44100\n\
START=0\n\
END=44100\n\
title=Line one\\\n\
Line two\n\
[CHAPTER]\n\
TIMEBASE=1/44100\n\
START=44100\n\
END=88200\n\
title=Literal backslash n n here\n";

    #[test]
    fn ffmpegs_own_dumped_output_parses_with_its_odd_timebase() {
        let out = parse(FFMPEG_DUMPED).expect("parses");
        assert_eq!(out.format, SidecarFormat::Ffmetadata);
        assert_eq!(out.chapters.len(), 2);
        // START=44100 in 1/44100 timebase is one second, and reading 44100 as
        // milliseconds would put it at 44.1 seconds.
        assert_eq!(out.chapters[1].start_ms, 1_000, "44100/44100 seconds");
        assert_eq!(out.tag("major_brand"), Some("M4A "), "trailing space kept");
        assert!(
            out.warnings.iter().any(|w| matches!(
                w,
                SidecarWarning::UnusualTimebase {
                    num: 1,
                    den: 44_100
                }
            )),
            "an audio timebase is unusual but perfectly valid, so it is reported: {:?}",
            out.warnings
        );
    }

    #[test]
    fn a_title_containing_a_newline_survives_the_round_trip() {
        // The writer escapes a newline as a backslash plus a real newline; the reader
        // has to rejoin those lines or the title is truncated at the first newline.
        let out = parse(FFMPEG_DUMPED).expect("parses");
        assert_eq!(out.chapters[0].title, "Line one\nLine two");

        // And a literal backslash, which the writer emits doubled, is not mistaken for
        // a continuation marker.
        assert_eq!(out.chapters[1].title, "Literal backslash n n here");
    }

    #[test]
    fn our_own_rendered_title_with_a_newline_reparses() {
        let c = SidecarChapters {
            chapters: vec![
                Chapter::new("Line one\nLine two", 0, 0),
                Chapter::new("Ends with a backslash \\", 1_000, 0),
            ],
            ..SidecarChapters::default()
        };
        let rendered = c.to_ffmetadata();
        assert_eq!(
            rendered.matches("[CHAPTER]").count(),
            2,
            "a newline must not have created a chapter:\n{rendered}"
        );
        let back = parse(&rendered).expect("our own output parses");
        assert_eq!(back.chapters.len(), 2);
        assert_eq!(back.chapters[0].title, "Line one\nLine two");
        assert_eq!(
            back.chapters[1].title, "Ends with a backslash \\",
            "a doubled backslash at end of line is a literal, not a continuation"
        );
    }

    #[test]
    fn a_stray_trailing_backslash_swallows_the_next_key() {
        // Documented hazard, asserted rather than defended against. A continuation
        // marker with nothing to continue joins the *next* line, so the following
        // section header stops being a header and its keys keep overwriting the chapter
        // they leaked into. The chapter count drops and the surviving title is the
        // *last* one. ffmpeg's reader behaves the same way, so no recovery is invented
        // that ffmpeg would not also perform.
        let text = ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\n\
                    title=Trailing slash \\\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=1000\n\
                    END=2000\ntitle=Second\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.chapters.len(), 1, "the second header was absorbed");
        assert_eq!(
            out.chapters[0].title, "Second",
            "the leaked keys overwrote the title"
        );
        assert_eq!(out.chapters[0].start_ms, 1_000, "and the start");
    }

    #[test]
    fn only_an_odd_run_of_backslashes_continues_a_line() {
        // Three ways to end a line with backslashes: one continues, two are an escaped
        // literal, three are an escaped literal followed by a continuation marker.
        assert!(ends_with_odd_backslash("a\\"), "one");
        assert!(!ends_with_odd_backslash("a\\\\"), "two");
        assert!(ends_with_odd_backslash("a\\\\\\"), "three");
        assert!(!ends_with_odd_backslash("a"));
        assert!(!ends_with_odd_backslash(""));

        let one = logical_lines("one\\\ntwo\nthree\n");
        assert_eq!(
            one,
            vec![(1, "one\ntwo".to_string()), (3, "three".to_string())],
            "one backslash continues, the newline is kept, and the line number is where \
             the logical line started"
        );

        let two = logical_lines("one\\\\\ntwo\n");
        assert_eq!(
            two,
            vec![(1, "one\\\\".to_string()), (2, "two".to_string())],
            "two backslashes are one escaped literal, so the line ends there"
        );

        let three = logical_lines("one\\\\\\\ntwo\n");
        assert_eq!(
            three,
            vec![(1, "one\\\\\ntwo".to_string())],
            "three: the escaped pair stays and the third still continues"
        );
    }

    #[test]
    fn a_known_duration_gives_the_last_chapter_a_real_end() {
        // Without a duration the last `END` equals its own `START`, which reads as a
        // zero-length chapter. A caller that knows the media length can say so.
        let without = SidecarChapters {
            chapters: vec![Chapter::new("One", 0, 0), Chapter::new("Two", 60_000, 0)],
            ..SidecarChapters::default()
        };
        assert!(
            without.to_ffmetadata().contains("END=60000"),
            "falls back to the chapter's own start"
        );

        let with = SidecarChapters {
            duration_ms: Some(180_000),
            ..without.clone()
        };
        let rendered = with.to_ffmetadata();
        assert!(
            rendered.contains("END=60000"),
            "the middle chapter is unchanged"
        );
        assert!(
            rendered.contains("END=180000"),
            "the last chapter reaches the end:\n{rendered}"
        );

        // A duration at or before the last chapter's start is no use, so it is ignored
        // rather than producing an `END` before `START`.
        let backwards = SidecarChapters {
            duration_ms: Some(60_000),
            ..with.clone()
        };
        assert!(
            backwards.to_ffmetadata().contains("END=60000"),
            "not a negative-length chapter"
        );

        // And the chapter count is unaffected either way.
        assert_eq!(parse(&rendered).expect("reparses").chapters.len(), 2);
    }

    #[test]
    fn a_title_ending_in_a_backslash_is_reported_because_ffmpeg_loses_a_chapter() {
        // Verified against ffmpeg: the reader strips the escaped pair's last backslash as
        // a continuation marker, swallows the next `[CHAPTER]` header, then discards the
        // out-of-order `START` — so two chapters become one, with a start time of zero,
        // and nothing is printed on stderr.
        let file = ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\n\
                    title=Ends with backslash \\\\\n\
                    [CHAPTER]\nTIMEBASE=1/1000\nSTART=1000\nEND=2000\ntitle=Second\n\
                    [CHAPTER]\nTIMEBASE=1/1000\nSTART=2000\nEND=3000\ntitle=Third\n";
        let out = parse(file).expect("parses");
        assert!(
            out.warnings
                .iter()
                .any(|w| matches!(w, SidecarWarning::TrailingBackslash { line: 6 })),
            "the hazardous line is named: {:?}",
            out.warnings
        );

        // This parser counts backslashes, so unlike ffmpeg it keeps every chapter.
        assert_eq!(
            out.chapters.len(),
            3,
            "all three survive, which is the whole point of counting"
        );
        assert_eq!(out.chapters[0].title, "Ends with backslash \\");
        assert_eq!(out.chapters[1].start_ms, 1_000, "and the times are right");
        assert_eq!(out.chapters[2].title, "Third");
    }

    #[test]
    fn ffmpeg_reads_back_is_false_only_for_a_trailing_backslash() {
        assert!(ffmpeg_reads_back("Plain Title"));
        assert!(ffmpeg_reads_back("Has = equals ; semi # hash"));
        assert!(ffmpeg_reads_back("Has \\ backslash in the middle"));
        assert!(ffmpeg_reads_back("Two\nLines"));
        assert!(ffmpeg_reads_back("Ends with space "));
        assert!(!ffmpeg_reads_back("Ends with backslash \\"));
        assert!(
            !ffmpeg_reads_back("\\"),
            "a lone backslash is a continuation too"
        );
        assert!(!ffmpeg_reads_back("Line one\nLine two\\"));
    }

    #[test]
    fn our_writer_and_reader_agree_even_where_ffmpeg_would_not() {
        // The write side cannot produce a file ffmpeg reads correctly for such a title,
        // but round-tripping through *this* crate must be exact — otherwise the crate
        // would be lossy in both directions, which would be its own defect.
        let c = SidecarChapters {
            chapters: vec![
                Chapter::new("Ends with backslash \\", 0, 0),
                Chapter::new("Second", 1_000, 0),
                Chapter::new("Third", 2_000, 0),
            ],
            ..SidecarChapters::default()
        };
        assert!(
            !ffmpeg_reads_back(&c.chapters[0].title),
            "and the caller is told so before writing"
        );
        let back = parse(&c.to_ffmetadata()).expect("our own output parses");
        assert_eq!(back.chapters.len(), 3);
        assert_eq!(back.chapters[0].title, "Ends with backslash \\");
        assert_eq!(back.chapters[1].start_ms, 1_000, "not swallowed");
        assert_eq!(back.chapters[2].title, "Third");
        assert_eq!(
            back.warnings.len(),
            1,
            "and the hazard is reported on re-read"
        );
        assert!(matches!(
            back.warnings[0],
            SidecarWarning::TrailingBackslash { .. }
        ));
    }

    #[test]
    fn timestamps_without_a_timebase_are_nanoseconds() {
        // Verified against ffmpeg: `END=60000000000` with no TIMEBASE reads back as
        // 60 s. Some older documentation says milliseconds, which would make this a
        // sixty-thousand-second chapter.
        let text = ";FFMETADATA1\n[CHAPTER]\nSTART=0\nEND=60000000000\ntitle=A\n\
                    [CHAPTER]\nSTART=60000000000\nEND=\ntitle=B\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.chapters.len(), 2);
        assert_eq!(out.chapters[1].start_ms, 60_000, "60 s, not 60000 s");
        assert!(
            out.warnings.contains(&SidecarWarning::NoTimebase),
            "and the assumption is reported, not silent"
        );
        assert!(out.warnings.contains(&SidecarWarning::EmptyEnd { line: 8 }));
    }

    #[test]
    fn whitespace_around_equals_is_reported_because_ffmpeg_drops_the_tag() {
        // Verified: `START =0` makes ffmpeg read the key as `START `, so the chapter
        // appears with an EMPTY title. Reporting it turns a silently wrong file into a
        // fixable one.
        let text =
            ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART =0\nEND =60000\ntitle = Spaced\n";
        let out = parse(text).expect("parses");
        assert!(
            out.warnings
                .iter()
                .any(|w| matches!(w, SidecarWarning::WhitespaceInKey { key, .. } if key.trim_end() == "START")),
            "the stray space before the equals is reported: {:?}",
            out.warnings
        );
    }

    #[test]
    fn escapes_round_trip() {
        // Verified against ffmpeg: this round-trips as `Has = equals ; semi # hash`.
        let text = ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\n\
                    title=Has \\= equals \\; semi \\# hash\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.chapters[0].title, "Has = equals ; semi # hash");

        // And our own rendering escapes it back.
        let rendered = out.to_ffmetadata();
        let again = parse(&rendered).expect("reparses");
        assert_eq!(again.chapters[0].title, out.chapters[0].title);
    }

    #[test]
    fn a_title_with_a_newline_is_escaped_rather_than_truncating_the_line() {
        let c = SidecarChapters {
            chapters: vec![Chapter::new("Line one\nLine two", 0, 0)],
            ..SidecarChapters::default()
        };
        let rendered = c.to_ffmetadata();
        assert_eq!(
            rendered.matches("[CHAPTER]").count(),
            1,
            "the newline must not have created a second chapter"
        );
        let back = parse(&rendered).expect("reparses");
        assert_eq!(back.chapters.len(), 1);
        assert_eq!(back.chapters[0].title, "Line one\nLine two");
    }

    #[test]
    fn global_tags_are_carried_through() {
        let text = ";FFMETADATA1\ntitle=The Long Now\nartist=A Writer\n\
                    [CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\ntitle=One\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.tag("title"), Some("The Long Now"));
        assert_eq!(
            out.tag("ARTIST"),
            Some("A Writer"),
            "keys are case-insensitive"
        );
        assert_eq!(out.tag("nope"), None);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let text = ";FFMETADATA1\n; a comment\n# another\n\n[CHAPTER]\n\
                    TIMEBASE=1/1000\nSTART=0\nEND=1000\ntitle=One\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.chapters.len(), 1);
        assert_eq!(out.chapters[0].title, "One");
    }

    #[test]
    fn timestamps_parse_in_every_documented_shape() {
        assert_eq!(parse_timestamp("0"), Some(0));
        assert_eq!(parse_timestamp("90"), Some(90_000));
        assert_eq!(parse_timestamp("1:30"), Some(90_000));
        assert_eq!(parse_timestamp("0:01:30"), Some(90_000));
        assert_eq!(parse_timestamp("1:00:00"), Some(3_600_000));
        assert_eq!(parse_timestamp("0:00:01.500"), Some(1_500));
        assert_eq!(parse_timestamp("0:00:01:500"), Some(1_500), "colon form");
        assert_eq!(
            parse_timestamp("00:00:01.5"),
            Some(1_500),
            "one decimal place"
        );
        assert_eq!(
            parse_timestamp("0:00 01:30"),
            Some(90_000),
            "space-separated"
        );
        assert_eq!(parse_timestamp("  1:30  "), Some(90_000));
    }

    #[test]
    fn a_nonsense_timestamp_is_none_not_a_zero() {
        assert_eq!(parse_timestamp(""), None);
        assert_eq!(parse_timestamp("abc"), None);
        assert_eq!(parse_timestamp("1:xx"), None);
        assert_eq!(
            parse_timestamp("0:00:01.1234567"),
            None,
            "too many decimals"
        );
    }

    #[test]
    fn format_timecode_is_the_inverse_of_parse_timestamp() {
        for ms in [0u64, 1, 999, 1_000, 90_000, 3_600_000, 86_399_999] {
            let text = format_timecode(ms);
            assert_eq!(
                parse_timestamp(&text),
                Some(ms),
                "`{text}` should round-trip"
            );
        }
        assert_eq!(format_timecode(90_000), "0:01:30.000");
        assert_eq!(format_timecode(3_600_000), "1:00:00.000");
    }

    #[test]
    fn the_chapterx_form_parses_with_names_in_either_order() {
        let text = "CHAPTER01=00:00:00.000\nCHAPTER01NAME=Opening\n\
                    CHAPTER02=00:10:21.000\nCHAPTER02NAME=Second\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.format, SidecarFormat::ChapterX);
        assert_eq!(out.chapters.len(), 2);
        assert_eq!(out.chapters[0].title, "Opening");
        assert_eq!(out.chapters[1].start_ms, 621_000, "10:21 is 621 s");

        let swapped = "CHAPTER02NAME=Second\nCHAPTER02=00:10:21.000\n\
                       CHAPTER01NAME=Opening\nCHAPTER01=00:00:00.000\n";
        let out = parse(swapped).expect("parses");
        assert_eq!(out.chapters.len(), 2);
        assert_eq!(out.chapters[0].title, "Opening");
    }

    #[test]
    fn our_chapterx_output_uses_sequential_numbers_because_they_are_required() {
        // MP4Box requires consecutive lines to carry the same X, so a gap would drop
        // every chapter after it.
        let c = SidecarChapters {
            chapters: vec![
                Chapter::new("One", 0, 0),
                Chapter::new("Two", 1_000, 0),
                Chapter::new("Three", 2_000, 0),
            ],
            ..SidecarChapters::default()
        };
        let text = c.to_chapter_x();
        let numbers: Vec<u32> = text
            .lines()
            .filter_map(|l| l.strip_prefix("CHAPTER"))
            .filter_map(|l| l.split('=').next())
            .filter_map(|n| n.trim_end_matches("NAME").parse().ok())
            .collect();
        assert_eq!(numbers, vec![1, 1, 2, 2, 3, 3]);

        let back = parse(&text).expect("our own output parses");
        assert_eq!(back.chapters.len(), 3);
        assert_eq!(back.chapters[2].title, "Three");
    }

    #[test]
    fn bare_timecodes_parse() {
        let text = "00:00:00 Opening\n00:02:08 Second\n00:05:06 Third\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.format, SidecarFormat::Timecode);
        assert_eq!(out.chapters.len(), 3);
        assert_eq!(out.chapters[1].start_ms, 128_000);
        assert_eq!(out.chapters[2].title, "Third");

        let back = parse(&out.to_timecode()).expect("our own output parses");
        assert_eq!(back.chapters.len(), 3);
    }

    #[test]
    fn a_zoomplayer_timestamp_needs_no_frame_rate() {
        let text = "AddChapterByTime(0,0,5,Opening)\nAddChapterByTime(0,1,0,Second)\n\
                    AddChapterBySeconds(90,Third)\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.format, SidecarFormat::ZoomPlayer);
        assert_eq!(out.chapters.len(), 3);
        assert_eq!(out.chapters[0].start_ms, 5_000);
        assert_eq!(out.chapters[1].start_ms, 60_000);
        assert_eq!(out.chapters[2].start_ms, 90_000);
    }

    #[test]
    fn a_frame_count_needs_a_frame_rate_and_is_not_guessed() {
        // AddChapter(nb_frames, name) cannot be converted without a frame rate the file
        // does not carry, so it is reported rather than assumed to be 25 or 30 fps —
        // either guess would put every chapter in the wrong place.
        let text = "AddChapter(150,Opening)\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.chapters.len(), 1);
        assert!(
            out.warnings.iter().any(|w| matches!(w, SidecarWarning::Unparsed { text, .. } if text.contains("frame rate"))),
            "reported: {:?}",
            out.warnings
        );
    }

    #[test]
    fn detection_picks_the_right_format() {
        assert_eq!(
            detect_format(REAL_FFMETADATA),
            Some(SidecarFormat::Ffmetadata)
        );
        assert_eq!(
            detect_format(";FFMETADATA1\ntitle=x\n"),
            Some(SidecarFormat::Ffmetadata)
        );
        assert_eq!(
            detect_format("CHAPTER01=0:00:00\nCHAPTER01NAME=One\n"),
            Some(SidecarFormat::ChapterX)
        );
        assert_eq!(
            detect_format("00:00:00 One\n00:01:00 Two\n"),
            Some(SidecarFormat::Timecode)
        );
        assert_eq!(
            detect_format("AddChapterBySeconds(1,One)\n"),
            Some(SidecarFormat::ZoomPlayer)
        );
        assert_eq!(detect_format("just some prose\n"), None);
        assert_eq!(detect_format(""), None);
    }

    #[test]
    fn an_unrecognisable_file_says_what_it_saw() {
        let err = parse("nothing here\njust words\n").expect_err("unrecognised");
        match err {
            SidecarError::Unrecognised { first_line } => {
                assert!(first_line.contains("nothing here"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn forcing_the_wrong_format_says_so_rather_than_returning_nothing() {
        let err = parse_as(REAL_FFMETADATA, SidecarFormat::Timecode).expect_err("wrong");
        assert!(matches!(err, SidecarError::WrongFormat { .. }));
    }

    #[test]
    fn out_of_order_chapters_are_reported_and_preserved() {
        // Preserved, not sorted: a file that lists them backwards is a fact about the
        // file, and silently reordering it hides the defect.
        let text = "00:05:00 Second\n00:00:00 First\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.chapters[0].start_ms, 300_000, "file order kept");
        assert!(
            out.warnings
                .iter()
                .any(|w| matches!(w, SidecarWarning::OutOfOrder { start_ms: 0, .. })),
            "and the problem is reported"
        );
        assert_eq!(out.sorted()[0].title, "First", "a sorted view is available");
    }

    #[test]
    fn an_untitled_chapter_is_kept_and_reported() {
        // A boundary with no name is still a boundary; dropping it loses a seek point.
        let text = "00:00:00\n00:01:00 Named\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.chapters.len(), 2);
        assert_eq!(out.chapters[0].title, "");
        assert!(out.warnings.contains(&SidecarWarning::Untitled { line: 1 }));
    }

    #[test]
    fn an_unusual_timebase_is_reported() {
        let text = ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000000000\n\
                    START=60000000000\nEND=\ntitle=A\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.chapters[0].start_ms, 60_000);
        assert!(out.warnings.iter().any(|w| matches!(
            w,
            SidecarWarning::UnusualTimebase {
                num: 1,
                den: 1_000_000_000
            }
        )));
    }

    #[test]
    fn a_nanosecond_timebase_does_not_overflow() {
        // `N * num * 1000` overflows u64 for exactly the nanosecond values this has to
        // get right: 3.6e16 ns is a 10 000-hour book, and times 1000 is 3.6e19, past
        // u64::MAX (1.8e19). So the conversion is done in u128.
        let text = ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000000000\n\
                    START=36000000000000000\nEND=\ntitle=A Long Book\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.chapters[0].start_ms, 36_000_000_000, "10 000 hours");
    }

    #[test]
    fn a_thirty_minute_book_in_nanoseconds_is_thirty_minutes() {
        // A regression guard on the arithmetic itself: 1800000000000 ns is 1800 s,
        // which is a million times less than the same digits read as milliseconds.
        let text = ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000000000\n\
                    START=1800000000000\nEND=\ntitle=Thirty Minutes\n";
        let out = parse(text).expect("parses");
        assert_eq!(out.chapters[0].start_ms, 1_800_000);
    }

    #[test]
    fn merge_keep_timestamps_prefers_existing_titles_when_counts_match() {
        let sidecar = SidecarChapters {
            chapters: vec![
                Chapter::new("Sidecar A", 0, 0),
                Chapter::new("Sidecar B", 5_000, 0),
            ],
            ..SidecarChapters::default()
        };
        let existing = vec![
            Chapter::new("Better A", 0, 0),
            Chapter::new("Better B", 9_000, 0),
        ];
        let merged = MergeStrategy::KeepTimestamps.merge(&sidecar, Some(&existing));
        assert_eq!(merged[0].title, "Better A");
        assert_eq!(merged[1].title, "Better B");
        assert_eq!(merged[1].start_ms, 5_000, "the sidecar's time wins");
    }

    #[test]
    fn merge_replace_all_takes_the_sidecar_wholesale() {
        let sidecar = SidecarChapters {
            chapters: vec![Chapter::new("Sidecar A", 0, 0)],
            ..SidecarChapters::default()
        };
        let existing = vec![
            Chapter::new("Better A", 0, 0),
            Chapter::new("Better B", 9_000, 0),
        ];
        let merged = MergeStrategy::ReplaceAll.merge(&sidecar, Some(&existing));
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].title, "Sidecar A");
    }

    #[test]
    fn merge_skip_on_mismatch_leaves_a_different_length_title_alone() {
        let sidecar = SidecarChapters {
            chapters: vec![Chapter::new("Sidecar A", 0, 0)],
            ..SidecarChapters::default()
        };
        let existing = vec![Chapter::new("A", 0, 0), Chapter::new("B", 9_000, 0)];
        let merged = MergeStrategy::SkipOnMismatch.merge(&sidecar, Some(&existing));
        assert_eq!(
            merged.len(),
            2,
            "untouched, because applying would misalign"
        );
        assert_eq!(merged[1].title, "B");

        // With matching lengths it does apply.
        let one = vec![Chapter::new("A", 0, 0)];
        let merged = MergeStrategy::SkipOnMismatch.merge(&sidecar, Some(&one));
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].title, "Sidecar A");
    }

    #[test]
    fn unescape_drops_the_backslash_and_keeps_the_next_character() {
        // Verified against ffmpeg: `\n` (backslash and letter n) reads back as plain
        // `n`, and a backslash before a real newline reads back as a newline. There is
        // no continuation rule — dropping the newline would lose the character.
        assert_eq!(ffmetadata_unescape("a\\=b"), "a=b");
        assert_eq!(ffmetadata_unescape("a\\nb"), "anb", "not a newline");
        assert_eq!(ffmetadata_unescape("a\\\nb"), "a\nb", "a real newline");
        assert_eq!(ffmetadata_unescape("a\\"), "a\\", "nothing to escape");
        assert_eq!(ffmetadata_unescape("plain"), "plain");
    }

    #[test]
    fn truncate_respects_character_boundaries() {
        assert_eq!(truncate("abc", 10), "abc");
        let t = truncate("é".repeat(50).as_str(), 5);
        assert!(t.ends_with('…'));
        assert!(t.len() <= 5 + '…'.len_utf8() + 1);
    }

    #[test]
    fn formats_round_trip_through_their_own_output() {
        let chapters = vec![
            Chapter::new("Opening", 0, 0),
            Chapter::new("A title; with = awkward \\ chars", 90_000, 0),
            Chapter::new("Chapter #3", 3_600_000, 0),
        ];
        for rendered in [
            SidecarChapters {
                chapters: chapters.clone(),
                ..SidecarChapters::default()
            }
            .to_ffmetadata(),
            SidecarChapters {
                chapters: chapters.clone(),
                ..SidecarChapters::default()
            }
            .to_chapter_x(),
            SidecarChapters {
                chapters: chapters.clone(),
                ..SidecarChapters::default()
            }
            .to_timecode(),
        ] {
            let back = parse(&rendered).expect("our own output parses");
            assert_eq!(back.chapters.len(), 3, "from:\n{rendered}");
            let back_titles: Vec<&str> = back.chapters.iter().map(|c| c.title.as_str()).collect();
            let want: Vec<&str> = chapters.iter().map(|c| c.title.as_str()).collect();
            assert_eq!(back_titles, want, "from:\n{rendered}");
        }
    }

    #[test]
    fn every_format_survives_being_empty() {
        // A sidecar with no chapters is a real thing — a user cleared the list — and
        // must not be an error.
        for text in [";FFMETADATA1\n", "CHAPTER01=\n"] {
            let out = parse(text).expect("empty sidecar is not an error");
            assert_eq!(out.chapters.len(), 0, "`{text}` yields no chapters");
        }
        // And the emptied-out line is reported rather than dropped in silence.
        let out = parse("CHAPTER01=0:00:00\nCHAPTER01NAME=One\nCHAPTER02=\n").expect("parses");
        assert_eq!(out.chapters.len(), 1);
        assert!(
            out.warnings
                .iter()
                .any(|w| matches!(w, SidecarWarning::Unparsed { line: 3, .. })),
            "the cleared line is reported: {:?}",
            out.warnings
        );
        assert!(!parse("")
            .expect_err("empty is unrecognised")
            .to_string()
            .is_empty());
    }
}
