//! Foundation of the Rust backend port: Python-compatible primitives
//! ([`compat`]), settings and data paths resolved like backend/config.py
//! ([`config`], [`paths`]), FastAPI-shaped errors ([`error`]), timestamps
//! ([`time`]) and file hashing ([`hash`]).
//!
//! The port is bug-compatible with the Python backend: anything a client or a
//! stored row can observe must come out byte for byte the same. The
//! `compat` functions are therefore verified against vectors that CPython
//! generates (`contracts/compat-vectors`, see tools/parity/export_vectors.py).
//! This crate does no I/O beyond reading files it is asked to hash.

pub mod compat;
pub mod config;
pub mod error;
pub mod hash;
pub mod paths;
pub mod time;
