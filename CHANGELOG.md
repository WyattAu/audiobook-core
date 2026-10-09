# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

### Fixed

- **An MP3's duration ignored the encoder's own delay and padding.** Every MP3 starts
  with a fixed encoder delay and is padded out to a whole frame, so the samples a file
  holds are longer than the audio in it, and LAME records both numbers in a tag at the
  end of the Xing/Info header. The reader ignored the tag, so every book was reported
  slightly longer than it is.

  Verified against ffmpeg on six files spanning mono and stereo, CBR and VBR, and
  MPEG-1 and MPEG-2: all six now match ffprobe's duration to the millisecond, where
  before all six were ~30 ms long. mutagen is wrong on all six — it requires the literal
  string `LAME` and ffmpeg writes `Lavc`, `Lavf` or `L3.99`, so it silently reports the
  untrimmed figure. All four are accepted here.

- **The MPEG-2 side-information width was one byte short, which cost 44% of the
  duration on the commonest audiobook encoding.** The offset of a Xing/Info tag from the
  start of its frame comes from the side-information width: 17 and 32 bytes for MPEG-1
  mono and stereo, 9 and 17 for MPEG-2. The MPEG-2 rows were 8 and 16. Reading them one
  byte short does not fail loudly — the tag simply reads as *absent*, so a file with a
  perfectly good frame count falls back to estimating the duration from a bitrate, and
  the bitrate it reads is the placeholder in the very frame the tag lives in.

  A 12-second 22.05 kHz mono audiobook — the encoding a spoken-word book actually ships
  in — was reported as 6.9 seconds, with a bitrate of 56 kbit/s against 32 kbit/s of real
  audio. It now reports 12.000 s and 32182 bps.

- **The declared byte count was used without removing the frame the tag lives in.**
  Xing's byte count covers that frame and its frame count does not, so every bitrate
  derived from the pair was one frame too large. Invisible on a long book, most of the
  error on a short one.

### Added

- **`LameTag`, `VBRITAG_OFFSET`, `AudioScan::samples_of` and `trimmed_duration_ms`**, so a
  caller can see *why* a duration is the length it is rather than only how long.
- **`MediaProbe::encoder_trim_applied`**, which is `false` for an MP4 and for an MP3 whose
  encoder said nothing. Defaulting it to `true` would let a container claim a trim no
  container has, so a caller comparing against a player can tell whether agreement was
  expected.
- **`examples/probecmp.rs`**, which prints a probe's verdict for a list of files. Every
  number it shows was compared by hand against ffprobe; the comparison is written down in
  this file rather than asserted in the example.

### Verified, and deliberately not done

- **A trim as long as the audio it describes is ignored rather than subtracted.** Old LAME
  versions wrote delay and padding on short, low-bitrate files that held no audio at all,
  and subtracting them reports a zero-length book. The untrimmed count stands and
  `encoder_trim_applied` stays `false`: a duration a few milliseconds wrong beats one that
  is empty, and the flag is how a caller finds out which it has.

### Changed

- **`probe_source` — the same probe, through bounded reads.** An MP3's whole story is in
  its first few kilobytes — tag, first frame, and the Xing/Info/LAME headers inside that
  frame — so the reads are bounded by the tag's own declared size plus one frame. An MP4's
  duration, metadata and `chpl` list are in its `moov`, which `read_top_level` finds by
  reading box headers only; the chapter track's samples are then fetched one bounded read
  each. No path holds a whole file, whatever its size, and every field agrees with
  [`MediaProbe::probe`] on the same file — asserted by tests, because a windowed reader
  that produced a *plausible* answer rather than the right one would be worse than the
  slow path it replaces.

### Fixed

- **The windowed MP3 path searched the audio window for a second ID3 tag it had already
  passed.** `first_frame` takes a container kind and skips a tag when it sees one, and the
  windowed path handed it the audio *after* the tag while telling it the file was tagged —
  so `find_id3v2` found nothing, `first_frame` returned nothing, and every tagged MP3 read
  as tagless: no title, no chapters, no duration. Found by the equivalence gate the same
  hour it was written, which is the argument for the gate.

## [0.5.0] - 2026-10-06

No API change; a behaviour change inherited from `id3-core`, and a big one for MP3.

### Fixed

- **ID3v2.3 tags now yield anything at all.** `MediaProbe` reads metadata through
  `id3_core::Tag::parse`, and every v2.3 tag failed to parse there \u{2014} v2.4's ten-byte
  frame header was used for v2.3's frames \u{2014} so the title, artist, album and chapters
  all came back empty. Nothing in the corpus caught it because ffmpeg, which writes every
  fixture, emits v2.4.

  `MediaProbe::chapters` on an MP3 is now covered by the conformance harness against
  ffprobe on three ID3v2.3 and `CHAP`-bearing shapes, none of which ffmpeg can produce.

### Notes

- `MediaProbe` reads MP3 chapters through `id3_core::Tag::resolved_chapters`, which orders
  by time rather than by TOC order. A `CTOC` whose top-level flag is misread therefore
  does not change the chapter list a player shows \u{2014} it changes nesting depth, which
  this crate does not model. See `id3-core` 0.2.0 for the flag defect itself and the
  harness's `UNCHECKED_FIELDS` for the gap that hides it here.

## [0.4.0] - 2026-10-06

Chapters arrive outside the audio file more often than not, so the sidecar formats the
tools actually exchange are now read and written.

### Added

- **`sidecar` module** — reads and writes the four chapter sidecar formats real tooling
  uses, with no dependency and no I/O:
  - ffmpeg's `;FFMETADATA1`, which is what `ffmpeg -i x -i chapters.ffmeta
    -map_metadata 1` consumes, so a library can *write* chapters without shipping a muxer.
  - MP4Box's and `mp4chaps`' `CHAPTER01`/`CHAPTER01NAME` form.
  - Bare `h:mm:ss[.ms] name` timecodes.
  - ZoomPlayer's `AddChapter`, `AddChapterBySeconds` and `AddChapterByTime`.

  Writers emit all three of the first, so the same chapter list can be handed to ffmpeg
  or to MP4Box.
- **`SidecarWarning`** — every warning here is a file a human edited and got nearly
  right, and reporting them turns a silently wrong chapter list into a fixable one.
  Worth knowing about:
  - `WhitespaceInKey` — a stray space before an `=` makes ffmpeg read the key as `START `
    and **discard the chapter's title**. Verified against ffmpeg: the chapter appears
    with an empty title and only a warning on stderr.
  - `NoTimebase` — with no `TIMEBASE`, ffmpeg reads `START`/`END` as **nanoseconds**.
    Verified by muxing `END=60000000000` and reading back 60 s; some older copies of the
    documentation say milliseconds, which would make it a 60 000-second chapter.
  - `TrailingBackslash` — see below.
- **`ffmpeg_reads_back(title)`** — whether a title survives a round trip through ffmpeg.
- **`MergeStrategy`** — `KeepTimestamps` (the default), `ReplaceAll` and
  `SkipOnMismatch`, for applying a sidecar to a title that already has chapters.
- **`SidecarChapters::duration_ms`** — lets a caller give the last chapter a real `END`
  instead of one equal to its own `START`.

### Fixed

- **A title ending in a backslash is now detected rather than silently corrupting a
  file.** ffmpeg's reader takes the escaped pair as a line continuation and swallows the
  *next* line: the following `[CHAPTER]` header disappears, the chapter count drops, and
  the survivor reports a start time of zero. Nothing appears on stderr. This crate counts
  backslashes instead, so all chapters survive, and reports the hazard so a caller can
  warn before handing the file to ffmpeg. Found by the conformance harness.

### Notes on the format, established by running ffmpeg rather than by reading about it

- A title containing a newline is written as a backslash at the end of the line followed
  by a real newline, and reads back as a newline. Reading the file line by line truncates
  the title *and* then trips over the remainder as a syntax error, so the parser rejoins
  continued lines first.
- ffmpeg dumps the **audio** timebase (`1/44100`), not `1/1000`, so a reader that assumes
  milliseconds places every chapter 44100x too late.
- Whitespace in a value is significant. `foo = bar` is a tag named `foo ` with the value
  ` bar`.
- `AddChapter(nb_frames, name)` needs a frame rate the format does not carry. It is
  reported rather than guessed at, because assuming 25 or 30 fps puts every chapter in
  the wrong place.

## [0.3.0] - 2026-10-05

Chapters now come from the QuickTime chapter track where a file has one, and the
harness's last unchecked field is closed.

### Added

- **`MediaProbe::chapter_disagreement`** — `Some` when a file carries both a Nero `chpl`
  box and a QuickTime chapter track and the two lists differ. A muxer can write each
  from a different source, and a library that silently prefers one shows the user a list
  the file contradicts — while a plain chapter comparison passes, because neither list is
  wrong on its own.
- **`MediaProbe::chapters` prefers the chapter track**, matching what a player shows.
  QuickTime, iTunes and the built-in iOS audiobook player read the track and ignore Nero
  chapters entirely.

### Changed

- **Chapters are reported in file order, not time order.** The old probe sorted them,
  which silently hid a producer that wrote its chapters backwards — a defect no consumer
  could then see. The integration test that had asserted the sorting now asserts that
  the file's order survives *and* that `Title::validate` reports the problem, which is
  the honest division of labour: the reader preserves what the file says, the validator
  judges it.

## [0.2.0] - 2026-10-05

Chapters can now span files, which is the shape real audiobook tooling produces.

### Added

- **`Chapter::parts`** — the parts a chapter occupies. A chapter is not a file: a
  book's chapter boundary frequently lands mid-file, because Audible and Librivox
  split on time rather than on file, so a 14-minute chapter can begin 3 minutes into
  an 18-minute MP3, and one chapter routinely runs across two files. Tools that build
  M4Bs express this as a three-level **parts → chapters → files** structure where a
  chapter names several source files.
  A model where a chapter lives inside exactly one file cannot represent that, and
  the failure is worse than a missing feature: the boundary is either dropped or
  attributed to the wrong file, and a seek lands in the wrong place with no error
  anywhere.
- **`chapter::concatenate_spanning`** — concatenates per-file chapters with the caller
  declaring, one bit per file, which files' last chapters run on. Where a span *ends*
  is derived rather than guessed: the first following file whose first chapter starts
  at zero, because that is where the chapter ends and none of that file's audio is its
  own. A next file whose first chapter starts *after* zero contributes its opening
  audio, so it is included.
- **`Title::from_flat_chapters`** — assembles a title from a flat chapter list plus
  one length per part, filing **each chapter under the part it starts in**. Building
  the parts by hand and filing a chapter under the part it merely *spans* is the easy
  mistake, and the consequence is a chapter whose whole-title offset comes from the
  wrong file.

### Changed

- **Chapter times are now part-local, not title-absolute.** `concatenate` preserves
  what each container stated and records which part each chapter came from;
  `Title::laid_out` computes the whole-title offsets, because they depend on the
  parts' lengths and nothing in a per-file container knows those. This is a **breaking
  change** to `Chapter::start_ms` semantics and the reason for the minor version bump
  while the crate is 0.x.
- **`concatenate_with_durations` is gone.** With the offsets moved to `laid_out` it
  took a parameter it did not use, and a signature that lies about where numbers come
  from is worse than one that is missing. The per-file lengths are now supplied once,
  to `Title::from_flat_chapters`.
- **`Title::with_part_at` renumbers chapters.** A chapter names its part by index,
  and a stale index points at a real but wrong file — so moving a part maps every
  chapter's `parts` through the same permutation before re-laying out.

### Fixed

Found while building the above, each caught by an invariant that did not exist before:

1. `Chapter::spanning` filtered out part 0 from the span it was given, so a chapter
   could never include the file it started in.
2. `concatenate_spanning` passed the *tail* of the span rather than the whole span,
   compounding the above.
3. `from_flat_chapters` filed an out-of-range chapter into the last part without
   renaming it, leaving its stored index disagreeing with the part holding it.
   Nothing is dropped — losing a boundary silently is the worst failure — and the span
   of a chapter naming parts that do not exist collapses to the one it was filed under.

## [0.1.0] - 2026-10-05

First release. `audiobook-core` is the estate's L1 audiobook layer: the place
where `id3-core` and `mp4-core'stop being container parsers and become a book.

### Added

- **`Title`** — one work, with metadata, parts, chapters, and provenance for both.
  Assembled from a single file (`from_probe`) or from several with real per-file
  lengths (`from_probes`), with `validate` catching the three ways a producer can
  make a title internally inconsistent.
- **`Part`** — a named section with a declared `duration_ms` distinct from the
  lower bound implied by its last chapter, plus `offset_ms` for its position in
  the title. A part whose chapters start at 0 spans nothing on its own, which is
  why the declared length is a separate field rather than a derived one.
- **`Chapter`** — a title and a start offset, file-local, with the flat
  `Title::chapters()` view title-absolute. The two are named differently on
  purpose; mixing them is the mistake a flat list invites.
- **`MediaProbe`** — duration, sample rate, channels, bitrate, VBR/CBR, title,
  artist, year, track, cover, and chapters, all without decoding a sample, and a
  `Confidence` that distinguishes a counted number from a derived one.
- **`mpeg`** — an MPEG-1/2/2.5 Layer I–III frame walker. An MP3 declares no
  duration anywhere, so one has to be derived: from a Xing/Info/VBRI frame count
  when the encoder wrote one, or from the bitrate and byte count when it did not.
  Both index tables, the padding bit, and the per-version `vbr_tag_offset` are
  modelled, because each is a place a naive reader is silently wrong.
- **`container`** — signature-based detection of MP3-with-ID3, bare MP3, and
  ISO-BMFF, with a bounded scan for a frame sync that is not at offset 0.

### Fixed during development

Six defects the tests caught, several of them the kind that produce plausible
wrong numbers rather than errors:

1. **`Title::matches` trimmed only one side.** Comparing a trimmed string against
   an untrimmed one silently fails on exactly the sloppy input the function exists
   to absorb, so two titles differing only in surrounding whitespace never
   matched.
2. **Assembly stored part lengths but never offsets.** Every part after the first
   reported its chapters at offset 0, so a seek bar jumped to the wrong file.
3. **`Part::span_ms` conflated a lower bound with a duration.** A part's chapters
   always start at 0, so the "span" was 0 for every file in a folder, making every
   part after the first unreachable. Split into `implied_span_ms` (the bound) and
   `duration_ms` (the fact).
4. **`Confidence::default()` was `Declared`.** A default-constructed probe has
   measured nothing, and defaulting it to `Declared` let a caller read a missing
   duration as a trusted one. The variant order is now load-bearing, since `Ord`
   derives from it and `from_probes` uses `max` to pick the most trustworthy
   probe's confidence.
5. **Bitrate arithmetic divided by a guarded zero.** A container whose `mvhd`
   carries a timescale and no elapsed time makes the denominator zero, and the
   library was trying to list that file. Now `checked_div`, reporting no bitrate.
6. **`AudioScan::duration_ms` read `frames_walked`.** A file that declares its
   frame count is never walked frame by frame, so that field is 0 and the probe
   reported a zero-length file for the most reliable input it could get.

Also fixed in the dependency while building this: `mp4-core` could not read a
conformant sample rate (`stsd` was not in the container set, and its full-box
header is eight bytes rather than four) and misread the `AudioSampleEntry` rate
field. Both are recorded in `mp4-core`'s changelog.

### Design notes worth knowing

- **A title with no chapters has none.** No fallback to "one chapter per file".
  Deriving that produces a UI that says "Chapter 1 … Chapter 12" for a book whose
  author wrote nine, and then caches the lie.
- **Per-file durations are required for correct offsets.** With none, every file's
  chapters land at 0. The API accepts their absence and the tests pin the
  difference, so the degradation is visible rather than silent.
- **Ordering files is the host's job.** This crate sees bytes, not filenames, so
  `with_part_at` takes an index. That is what a lexical sort of `10.mp3, 2.mp3`
  needs, and it re-derives every offset after the move.

### Known limitations

- **No decoding.** No PCM, and no sample-accurate seek position — both need a
  decoder, which the estate does not have.
- **MPEG Layer I/II frames are parsed but not chapter-tested against real files.**
  Only Layer III appears in the tests; the walker handles all three because the
  format tables do, but only Layer III is exercised end to end.
- **`CTOC` nesting is not turned into parts.** `id3-core` resolves a `CTOC` into
  ordered chapters; this crate flattens them into one part rather than building a
  nested part hierarchy, which is a real loss for a multi-disc edition. The next
  piece of work.
- **No sidecar chapter formats.** A `.txt` or FFMetadata chapter file alongside the
  audio, an EPUB table of contents, or an Audnex API response are all chapter sources
  `audiobook-forge` supports and this crate does not. A `.txt` file needs no
  dependency and is the obvious next addition.
- **No cover-art validation.** Bytes are passed through raw; `media-kit` owns
  checking that they decode.
- **The frame walk is capped at 16 M frames.** Long enough for a 200 MB file at the
  shortest legal frame length, short enough to terminate on corrupt input.