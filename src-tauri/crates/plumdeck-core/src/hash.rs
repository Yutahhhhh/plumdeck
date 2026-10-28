//! File hashing as workflow_service._sha256 does it: SHA-256 over 1 MiB reads,
//! optionally refusing files larger than a byte limit.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Read size used by the Python backend.
pub const CHUNK_SIZE: usize = 1024 * 1024;

#[derive(Debug)]
pub enum HashError {
    Io(io::Error),
    /// More than `limit` bytes were read (Python raises ValueError).
    TooLarge,
}

impl fmt::Display for HashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::TooLarge => f.write_str("ファイルが許可されたサイズを超えています"),
        }
    }
}

impl std::error::Error for HashError {}

impl From<io::Error> for HashError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Lowercase hex SHA-256 of everything `reader` yields.
pub fn sha256_reader(mut reader: impl Read, limit: Option<u64>) -> Result<String, HashError> {
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut total: u64 = 0;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        total += read as u64;
        if limit.is_some_and(|limit| total > limit) {
            return Err(HashError::TooLarge);
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

/// Lowercase hex SHA-256 of a file.
pub fn sha256_file(path: &Path, limit: Option<u64>) -> Result<String, HashError> {
    sha256_reader(File::open(path)?, limit)
}
