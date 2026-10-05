# audiobook-core

Audiobook domain model for Rust — format-independent titles, parts and chapters
assembled from MP3 (`id3-core`) and MP4/M4B (`mp4-core`).

**Layer:** L1 · **Estate deps:** `id3-core`, `mp4-core` · **External:** none ·
`no_std` + `alloc`

## Why

An audiobook arrives as any of:

| On disk | Chapters | Chapter times | Duration |
|---|---|---|---|
| one `.m4b` | one `chpl` list | already absolute | declared in `mvhd` |
| one `.mp3` | one `CHAP` list | already absolute | **not declared anywhere** |
| a folder of `.mp3` | one list *per file* | per file, needs offsetting | sum of per-file lengths |

A library that special-cases per format has three code paths that disagree about
what a chapter is, and nowhere to put a row. `Title` is that place: one work,
however it was packaged, with its parts, chapters, and duration.

## What it does

- **`MediaProbe`** — duration, sample rate, channels, bitrate, VBR or CBR, title,
  artist, album, year, track, cover, chapters. All without decoding a sample.
- **`mpeg`** — an MPEG-1/2/2.5 Layer I–III frame walker, because an MP3 has no
  duration field and one has to be derived. Xing/Info/VBRI frame counts when the
  encoder wrote one; bitrate and byte count when it did not.
- **`Title`** — metadata, parts, chapters, provenance, and `validate` for the three
  ways a producer can make a title internally inconsistent.
- **`container`** — signature-based detection, because an M4B is regularly named
  `.mp3` after a bad download.

## What it deliberately will not do

- **Not invent structure.** A title with no chapters reports
  `ChapterSource::None`. There is no fallback to "one chapter per file": that
  shows up in a player's UI as "Chapter 1 … Chapter 12" for a book whose author
  wrote nine, and then gets cached and re-exported.
- **Not decode.** No PCM, no sample-accurate seek. Both need a decoder.
- **Not decide file order.** This crate sees bytes, not filenames.

## Install

```toml
[dependencies]
audiobook-core = "0.1"
```

## Example

```rust
use audiobook_core::{MediaProbe, Title};

// One file — an M4B, an MP3, or a podcast episode.
let data = std::fs::read("book.m4b")?;
let probe = MediaProbe::probe(&data);

println!("{:?} · {:?}", probe.title, probe.duration_display());
if let Some(n) = probe.frame_count {
    println!("{n} frames, declared");
}

let title = Title::from_probe(&probe, "book.m4b", "unknown");
for c in title.chapters() {
    println!("{:>10} ms  {}", c.start_ms, c.title);
}
title.validate()?;
```

Several files, with the real per-file lengths that make the offsets correct:

```rust
use audiobook_core::{MediaProbe, Title};

// Probe each file's bytes. `durations[i]` is file `i`'s length in milliseconds —
// measured, not guessed; without it the concatenated chapter offsets collapse to
// zero.
let mut probes = Vec::new();
let mut durations = Vec::new();
for entry in std::fs::read_dir("Book/")?.filter_map(Result::ok) {
    let bytes = std::fs::read(entry.path())?;
    durations.push(measure_length_ms(&bytes)?);
    probes.push(MediaProbe::probe(&bytes));
}

// A folder's parts each carry their file's own length; offsets follow from it.
let title = Title::from_probes(&probes, &durations, "The Book", "A. Writer");
assert_eq!(title.parts.len(), probes.len());
assert!(title.validate().is_ok());

// Which file is playing at a given time — the question a file list answers.
if let Some(part) = title.part_at(1_200_000) {
    println!("now in: {}", part.title);
}
```

Seeking:

```rust
use audiobook_core::Title;

// `chapter_at` takes a time relative to the *whole title*, not to a part, and
// returns the borrowed chapter — whose `start_ms` stays file-local, so a caller
// that needs the absolute number uses `Title::chapters()`.
let title: Title = /* ... */;
if let Some(chapter) = title.chapter_at(1_200_000) {
    println!("{}", chapter.title);
}
```

## Guarantees

- **Probing never fails.** Bytes that cannot be identified yield a probe with
  `Confidence::Unknown` and nothing set, so a host can list a folder containing one
  unreadable file without failing the listing.
- **A counted number is distinguishable from a derived one.** Every field that came
  from a declared frame count or a container duration is paired with
  `Confidence::Declared`; every field derived from a bitrate and a byte count is
  `Confidence::Derived`.
- **Padding is read, not assumed.** A padded Layer III frame is one byte longer,
  which a bitrate-only duration derivation gets wrong by up to 1.5%. The tests pin
  the drift in both directions.
- **`Xing` and `Info` are told apart.** `Info` means an encoder wrote a count on a
  CBR stream; reporting it as VBR mislabels the file.
- **No panics in library code** — `deny(panic)`, `deny(indexing_slicing)`, and
  `deny(unwrap_used)` are on crate-wide, and every bitrate division is checked.
- **`no_std` + `alloc`**, verified on `thumbv7em-none-eabihf`.

## Gates

- `cargo test` — 108 tests (84 unit, 2 property, 23 integration, 1 doc)
- `cargo clippy --all-features --all-targets -- -D warnings`
- `cargo fmt --check`
- `cargo check --target thumbv7em-none-eabihf --no-default-features`
- estate `feature-compat` and layer gates

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.