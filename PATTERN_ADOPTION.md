# PATTERN_ADOPTION

Relationship to the WyattAu pattern canon
([pattern-standards](https://github.com/WyattAu/pattern-standards)),
validated by `scripts/adoption-check.py`. Evidence paths are relative to
this repo root.

| Pattern | Status | Evidence |
|---|---|---|
| T1 | adopted | src/probe.rs:111 (bytes parse once into a typed `MediaProbe`; callers query the result, no re-validation) |
| T13 | adopted | src/lib.rs:24 (byte-slice API, no file I/O and no decoding; the host owns the imperative shell) |
| T40 | partial | src/mpeg.rs:426 (frame walk scans `&[u8]` in place without decoding; header structs are copied out, so no zero-copy handoff) |
| P64 | adopted | src/chapter.rs:360 (two `proptest!` suites, 64 cases each: per-file offset flattening and chapter-name roundtrip) |
| P72 | adopted | src/error.rs:10 (crate-wide `#[non_exhaustive]` `AudiobookError`; every fallible API returns it) |
| P93 | partial | src/probe.rs:107 (unidentifiable bytes yield a `Confidence::Unknown` probe instead of an error, so a folder listing survives one bad file; probe path only) |

## Omitted patterns

T46, T28, and other catalog entries have no relationship to this crate
and are omitted per references/adoption-format.md rule 6 (R6: no named
failure mode, no adoption).
