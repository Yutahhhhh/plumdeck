//! Error types.

use std::fmt;

/// What part of the input this crate cannot decode yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// HE-AAC (SBR, optionally PS). `implicit` is true when the stream only
    /// looked like HE-AAC from its rates (spec 4.2), not from an explicit
    /// signal. An OS-decoder fallback is planned (W1-06).
    HeAac { ps: bool, implicit: bool },
    /// Opus (the decoder crate is chosen elsewhere, W0-11).
    Opus,
    /// The container was not recognised.
    Container,
    /// The container is known but the codec is not decodable here.
    Codec(String),
    /// The decoder library rejected the stream for another reason.
    Other(String),
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeAac { ps, implicit } => write!(
                f,
                "HE-AAC{} ({} signalling)",
                if *ps { " v2" } else { "" },
                if *implicit { "implicit" } else { "explicit" }
            ),
            Self::Opus => f.write_str("Opus"),
            Self::Container => f.write_str("unrecognised container"),
            Self::Codec(c) => write!(f, "codec {c}"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

/// Errors from probing and decoding.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported: {0}")]
    Unsupported(Unsupported),
    #[error("malformed input: {0}")]
    Malformed(String),
    #[error("no audio track")]
    NoAudioTrack,
}

impl DecodeError {
    pub(crate) fn malformed(msg: impl Into<String>) -> Self {
        Self::Malformed(msg.into())
    }
}

pub type Result<T, E = DecodeError> = std::result::Result<T, E>;
