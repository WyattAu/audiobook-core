//! Probe files and print what the reader makes of them. A development aid, not
//! part of the crate: every field it prints is compared by hand against an
//! external tool, and the comparison is written down in the CHANGELOG rather
//! than asserted here.
//!
//! ```text
//! cargo run --example probecmp -- book1.mp3 book2.m4b
//! ffprobe -v error -show_entries format=duration -of csv=p=0 book1.mp3
//! ```

fn main() {
    for path in std::env::args().skip(1) {
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("{path}: cannot read");
            continue;
        };
        let probe = audiobook_core::MediaProbe::probe(&bytes);
        println!(
            "{path}  dur={:?}ms trim={} conf={:?} br={:?} vbr={:?}",
            probe.duration_ms,
            probe.encoder_trim_applied,
            probe.confidence,
            probe.bitrate_bps,
            probe.variable_bitrate
        );
    }
}
