# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

## [0.1.0] - 2026-10-05

First release. `audiobook-core` is the estate's L1 audiobook layer: the place
where `id3-core` and `mp4-core` stop being container parsers and become a book.

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
  nested part hierarchy, which is a real loss for a multi-disc edition.
- **No cover-art validation.** Bytes are passed through raw; `media-kit` owns
  checking that they decode.
- **The frame walk is capped at 16 M frames.** Long enough for a 200 MB file at the
  shortest legal frame length, short enough to terminate on corrupt input.