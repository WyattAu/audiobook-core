//! The crate-wide error type.

use alloc::string::String;

use crate::container::ContainerKind;

/// Errors from assembling or validating a title.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudiobookError {
    /// The bytes are neither an MP3 with an ID3 tag nor an MP4.
    NotAudiobook {
        /// What was detected, if anything.
        detected: Option<ContainerKind>,
    },
    /// Two chapters overlap, or are out of order in a way that would make
    /// playback ambiguous.
    ChaptersOverlap {
        /// The earlier chapter's title.
        first: String,
        /// The later chapter's title.
        second: String,
        /// The later chapter's start, in milliseconds.
        at_ms: u64,
    },
    /// A chapter starts past the end of the audio.
    ChapterPastEnd {
        /// The chapter's title.
        title: String,
        /// Where it starts, in milliseconds.
        start_ms: u64,
        /// The audio's duration, in milliseconds.
        duration_ms: u64,
    },
    /// Two parts claim the same index or title.
    DuplicatePart {
        /// The repeated part title.
        title: String,
    },
    /// The underlying container reader failed.
    Container {
        /// Which container was being read.
        kind: ContainerKind,
        /// The reader's own error, rendered as text so this type stays `Clone`.
        reason: String,
    },
    /// A required field was empty where the format requires one.
    MissingField {
        /// The field name.
        field: &'static str,
    },
}

impl core::fmt::Display for AudiobookError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AudiobookError::NotAudiobook { detected } => match detected {
                Some(k) => write!(f, "not an audiobook: detected {k} with no usable metadata"),
                None => write!(f, "not an audiobook: no ID3 tag and no MP4 box structure"),
            },
            AudiobookError::ChaptersOverlap {
                first,
                second,
                at_ms,
            } => write!(f, "chapter `{second}` at {at_ms} ms overlaps `{first}`"),
            AudiobookError::ChapterPastEnd {
                title,
                start_ms,
                duration_ms,
            } => write!(
                f,
                "chapter `{title}` starts at {start_ms} ms, past the {duration_ms} ms of audio"
            ),
            AudiobookError::DuplicatePart { title } => {
                write!(f, "duplicate part `{title}`")
            }
            AudiobookError::Container { kind, reason } => {
                write!(f, "{kind} container could not be read: {reason}")
            }
            AudiobookError::MissingField { field } => {
                write!(f, "required field `{field}` is empty")
            }
        }
    }
}

impl core::error::Error for AudiobookError {}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )]
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn display_is_stable() {
        assert_eq!(
            AudiobookError::NotAudiobook { detected: None }.to_string(),
            "not an audiobook: no ID3 tag and no MP4 box structure"
        );
        assert_eq!(
            AudiobookError::ChaptersOverlap {
                first: String::from("One"),
                second: String::from("Two"),
                at_ms: 500,
            }
            .to_string(),
            "chapter `Two` at 500 ms overlaps `One`"
        );
        assert_eq!(
            AudiobookError::MissingField { field: "title" }.to_string(),
            "required field `title` is empty"
        );
    }

    #[test]
    fn is_std_error() {
        fn assert_error<E: core::error::Error>(_: &E) {}
        assert_error(&AudiobookError::DuplicatePart {
            title: String::from("x"),
        });
    }
}
