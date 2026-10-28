//! `GET /api/stream` and `GET /api/metadata`
//! (backend/api/routers/filesystem.py). `POST /api/fs/list` is not ported
//! (Explorer removed, deviation D-15); `PATCH /api/metadata/update` and the
//! artwork lookup arrive with S8.
//!
//! `/api/stream` is Starlette 1.6's FileResponse: `accept-ranges`,
//! `content-length`, `last-modified`, an `etag` of md5("{mtime}-{size}"),
//! single ranges as 206, several as `multipart/byteranges` (merged when they
//! overlap), 416 for a start past the end, 400 for a malformed header, and
//! `If-Range` honored against the last-modified or etag value. The
//! Content-Type comes from a fixed table (deviation D-16).

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use md5::{Digest, Md5};
use plumdeck_core::compat::pyfmt::float_repr;
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_core::compat::text::{is_py_space, py_lower, py_strip};
use plumdeck_core::error::ApiError;
use plumdeck_db::array_codec::unpack_u8_waveform;
use plumdeck_db::repos::tracks::{get_analysis, get_track, has_completed_analysis, BigInt};
use plumdeck_domain::pychars::decimal_value;
use plumdeck_media::mime::file_media_type;
use plumdeck_media::paths::resolve_path;
use plumdeck_media::tags::extract_full_metadata;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::query::Params;
use crate::response::{boolean, dict, float, opt, text, PyJson};
use crate::{server_error, AppState, HttpError};

/// `FileResponse.max_ranges`.
const MAX_RANGES: usize = 100;

fn plain(status: StatusCode, body: &str) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body.to_owned()).into_response()
}

/// Python `int(text)` for a Range bound: whitespace, sign, Unicode digits,
/// underscores between digits. `None` is a ValueError.
fn py_int(text: &str) -> Option<i128> {
    let text = text.trim_matches(is_py_space);
    let (negative, digits) = match text.chars().next() {
        Some('-') => (true, &text[1..]),
        Some('+') => (false, &text[1..]),
        _ => (false, text),
    };
    let chars: Vec<char> = digits.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut value: i128 = 0;
    for (index, c) in chars.iter().enumerate() {
        if *c == '_' {
            let between = index > 0 && index + 1 < chars.len() && chars[index - 1] != '_' && chars[index + 1] != '_';
            if !between {
                return None;
            }
            continue;
        }
        let digit = decimal_value(*c)?;
        value = value.saturating_mul(10).saturating_add(i128::from(digit));
    }
    Some(if negative { -value } else { value })
}

/// Why a Range header is not served.
#[derive(Debug, PartialEq)]
enum RangeError {
    /// 400 with this text.
    Malformed(&'static str),
    /// 416.
    NotSatisfiable,
}

/// `_parse_range_header`: `Ok(empty)` means "serve the whole file".
fn parse_range_header(header: &str, size: i128) -> Result<Vec<(i128, i128)>, RangeError> {
    let Some((units, ranges)) = header.split_once('=') else {
        return Err(RangeError::Malformed("Malformed range header."));
    };
    if py_lower(py_strip(units)) != "bytes" {
        return Err(RangeError::Malformed("Only support bytes range"));
    }
    if ranges.matches(',').count() + 1 > MAX_RANGES {
        return Ok(Vec::new());
    }
    let mut parsed = Vec::new();
    for part in ranges.split(',') {
        let part = py_strip(part);
        if part.is_empty() || part == "-" || !part.contains('-') {
            continue;
        }
        let (start_text, end_text) = part.split_once('-').expect("has a dash");
        let (start_text, end_text) = (py_strip(start_text), py_strip(end_text));
        let start = if start_text.is_empty() {
            match py_int(end_text) {
                Some(end) => (size - end).max(0),
                None => continue,
            }
        } else {
            match py_int(start_text) {
                Some(start) => start,
                None => continue,
            }
        };
        let end = if !start_text.is_empty() && !end_text.is_empty() {
            match py_int(end_text) {
                Some(end) if end < size => end + 1,
                Some(_) => size,
                None => continue,
            }
        } else {
            size
        };
        parsed.push((start, end));
    }
    if parsed.is_empty() {
        return Err(RangeError::Malformed("Range header: range must be requested"));
    }
    if parsed.iter().any(|(start, _)| !(0 <= *start && *start < size)) {
        return Err(RangeError::NotSatisfiable);
    }
    if parsed.iter().any(|(start, end)| start >= end) {
        return Err(RangeError::Malformed("Range header: start must be less than end"));
    }
    if parsed.len() == 1 {
        return Ok(parsed);
    }
    parsed.sort();
    let mut merged = vec![parsed[0]];
    for (start, end) in parsed.into_iter().skip(1) {
        let last = merged.last_mut().expect("non-empty");
        if start <= last.1 {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    Ok(merged)
}

/// `email.utils.formatdate(mtime, usegmt=True)`.
fn http_date(mtime: f64) -> String {
    let seconds = mtime.floor() as i64;
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|time| time.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
        .unwrap_or_default()
}

/// Stat headers of FileResponse.
struct FileStat {
    size: i128,
    last_modified: String,
    etag: String,
}

fn file_stat(metadata: &std::fs::Metadata) -> FileStat {
    #[cfg(unix)]
    let mtime = {
        use std::os::unix::fs::MetadataExt;
        // CPython: (double)st_mtime + st_mtime_nsec * 1e-9.
        metadata.mtime() as f64 + metadata.mtime_nsec() as f64 * 1e-9
    };
    #[cfg(not(unix))]
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0.0, |elapsed| elapsed.as_secs() as f64 + f64::from(elapsed.subsec_nanos()) * 1e-9);
    let size = i128::from(metadata.len());
    let digest = Md5::digest(format!("{}-{}", float_repr(mtime), size).as_bytes());
    let etag = format!("\"{}\"", digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>());
    FileStat { size, last_modified: http_date(mtime), etag }
}

/// The bytes `start..end` of the file as a stream (`bytes=0-` is the whole
/// file, which browsers ask for when they start playback).
async fn open_span(path: &PathBuf, start: i128, end: i128) -> std::io::Result<tokio::io::Take<tokio::fs::File>> {
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(start as u64)).await?;
    Ok(file.take((end - start) as u64))
}

/// The bytes `start..end` in memory (the parts of a multipart answer).
async fn read_span(path: &PathBuf, start: i128, end: i128) -> std::io::Result<Vec<u8>> {
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(start as u64)).await?;
    let mut out = vec![0u8; (end - start) as usize];
    file.read_exact(&mut out).await?;
    Ok(out)
}

/// `secrets.token_hex(13)`: 26 random hex digits.
fn boundary() -> String {
    let mut digits = uuid::Uuid::new_v4().simple().to_string();
    digits.truncate(26);
    digits
}

/// FileResponse for an existing path.
async fn file_response(path: PathBuf, request: &HeaderMap) -> Response {
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) => metadata,
        Err(error) => return server_error(&error),
    };
    if !metadata.is_file() {
        return server_error(&format!("File at path {} is not a file.", path.display()));
    }
    let stat = file_stat(&metadata);
    let media_type = file_media_type(&path.to_string_lossy());
    let content_type = if media_type.starts_with("text/") { format!("{media_type}; charset=utf-8") } else { media_type.to_owned() };
    // The stat headers every answer but the errors carries.
    let base = |status: StatusCode, content_type: &str, length: i128| {
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, length.to_string())
            .header(header::LAST_MODIFIED, stat.last_modified.clone())
            .header(header::ETAG, stat.etag.clone())
    };
    // Starlette reads headers as Latin-1 and takes the first value.
    let latin1 = |name: header::HeaderName| request.get(name).map(|value| value.as_bytes().iter().map(|&b| char::from(b)).collect::<String>());
    let range = latin1(header::RANGE);
    let if_range = latin1(header::IF_RANGE);
    let use_range = match (&range, &if_range) {
        (None, _) => false,
        (Some(_), Some(if_range)) => *if_range == stat.last_modified || *if_range == stat.etag,
        (Some(_), None) => true,
    };
    let ranges = if use_range {
        match parse_range_header(range.as_deref().expect("range"), stat.size) {
            Ok(ranges) => ranges,
            Err(RangeError::Malformed(message)) => return plain(StatusCode::BAD_REQUEST, message),
            Err(RangeError::NotSatisfiable) => {
                let mut response = plain(StatusCode::RANGE_NOT_SATISFIABLE, "");
                if let Ok(value) = HeaderValue::from_str(&format!("bytes */{}", stat.size)) {
                    response.headers_mut().insert(header::CONTENT_RANGE, value);
                }
                return response;
            }
        }
    } else {
        Vec::new()
    };
    match ranges.as_slice() {
        [] => {
            let file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(error) => return server_error(&error),
            };
            let stream = tokio_util::io::ReaderStream::with_capacity(file, 64 * 1024);
            base(StatusCode::OK, &content_type, stat.size)
                .body(Body::from_stream(stream))
                .unwrap_or_else(|error| server_error(&error))
        }
        [(start, end)] => match open_span(&path, *start, *end).await {
            Ok(reader) => base(StatusCode::PARTIAL_CONTENT, &content_type, end - start)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{}/{}", end - 1, stat.size))
                .body(Body::from_stream(tokio_util::io::ReaderStream::with_capacity(reader, 64 * 1024)))
                .unwrap_or_else(|error| server_error(&error)),
            Err(error) => server_error(&error),
        },
        many => {
            let boundary = boundary();
            let mut body = Vec::new();
            for (start, end) in many {
                body.extend_from_slice(
                    format!(
                        "--{boundary}\r\nContent-Type: {content_type}\r\nContent-Range: bytes {start}-{}/{}\r\n\r\n",
                        end - 1,
                        stat.size
                    )
                    .as_bytes(),
                );
                match read_span(&path, *start, *end).await {
                    Ok(bytes) => body.extend_from_slice(&bytes),
                    Err(error) => return server_error(&error),
                }
                body.extend_from_slice(b"\r\n");
            }
            body.extend_from_slice(format!("--{boundary}--").as_bytes());
            let multipart = format!("multipart/byteranges; boundary={boundary}");
            base(StatusCode::PARTIAL_CONTENT, &multipart, body.len() as i128)
                .body(Body::from(body))
                .unwrap_or_else(|error| server_error(&error))
        }
    }
}

/// `GET /api/stream?path=...`: the audio file for playback.
pub async fn stream(uri: Uri, headers: HeaderMap) -> Response {
    let mut params = Params::from_uri(&uri);
    let path = params.required_str("path");
    if let Err(response) = params.finish() {
        return response;
    }
    let path = path.expect("validated");
    let resolved = tokio::task::spawn_blocking(move || resolve_path(&path)).await;
    match resolved {
        Ok(Some(resolved)) => file_response(PathBuf::from(resolved), &headers).await,
        Ok(None) => HttpError(ApiError::new(404, "File not found")).into_response(),
        Err(error) => server_error(&error),
    }
}

/// `GET /api/metadata?track_id=...`: tags read from the file plus the
/// stored BPM, key, waveform and analyzed flag.
pub async fn track_metadata(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let track_id = params.required_int("track_id");
    if let Err(response) = params.finish() {
        return response;
    }
    let track_id = BigInt(track_id.expect("validated"));
    let db = state.db.clone();
    let found = state
        .blocking(move || {
            let conn = db.lease()?;
            let Some(track) = get_track(&conn, track_id)? else { return Ok(None) };
            let Some(resolved) = resolve_path(&track.filepath) else { return Ok(None) };
            let analysis = get_analysis(&conn, track.id)?;
            drop(conn);
            let tags = extract_full_metadata(std::path::Path::new(&resolved));
            Ok(Some((track, analysis, tags)))
        })
        .await;
    let (track, analysis, tags) = match found {
        Ok(Some(found)) => found,
        Ok(None) => return HttpError(ApiError::new(404, "Track not found or file missing")).into_response(),
        Err(response) => return response,
    };
    let peaks = analysis.as_ref().map(|a| unpack_u8_waveform(a.waveform_u8.as_deref())).unwrap_or_default();
    let analyzed = has_completed_analysis(&track, analysis.as_ref());
    PyJson(dict([
        ("title", opt(tags.title, text)),
        ("artist", opt(tags.artist, text)),
        ("album", opt(tags.album, text)),
        ("lyrics", text(tags.lyrics)),
        ("artwork", opt(tags.artwork, text)),
        ("bpm", opt(track.bpm, float)),
        ("key", opt(track.key, text)),
        ("waveform_peaks", PyValue::List(peaks.into_iter().map(float).collect())),
        ("analyzed", boolean(analyzed)),
    ]))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_headers_follow_starlette() {
        assert_eq!(parse_range_header("bytes=0-9", 100), Ok(vec![(0, 10)]));
        assert_eq!(parse_range_header("bytes=-10", 100), Ok(vec![(90, 100)]));
        assert_eq!(parse_range_header("bytes=95-", 100), Ok(vec![(95, 100)]));
        assert_eq!(parse_range_header("bytes=95-500", 100), Ok(vec![(95, 100)]));
        assert_eq!(parse_range_header(" BYTES =0-1, 1-2,5-9", 100), Ok(vec![(0, 3), (5, 10)]));
        assert_eq!(parse_range_header("bytes=-200", 100), Ok(vec![(0, 100)]));
        assert_eq!(parse_range_header("bytes=100-", 100), Err(RangeError::NotSatisfiable));
        assert_eq!(parse_range_header("bytes=5-2", 100), Err(RangeError::Malformed("Range header: start must be less than end")));
        assert_eq!(parse_range_header("bytes=x-y", 100), Err(RangeError::Malformed("Range header: range must be requested")));
        assert_eq!(parse_range_header("items=0-1", 100), Err(RangeError::Malformed("Only support bytes range")));
        assert_eq!(parse_range_header("0-1", 100), Err(RangeError::Malformed("Malformed range header.")));
        assert_eq!(parse_range_header(&format!("bytes={}", vec!["0-1"; 101].join(",")), 100), Ok(vec![]));
        assert_eq!(parse_range_header("bytes=１-２", 100), Ok(vec![(1, 3)]));
        assert_eq!(parse_range_header("bytes=1_0-2_0", 100), Ok(vec![(10, 21)]));
        assert_eq!(parse_range_header("bytes=0-1", 0), Err(RangeError::NotSatisfiable));
    }

    #[test]
    fn http_dates_and_boundaries() {
        assert_eq!(http_date(0.5), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(86_400.0 * 365.0 + 3_661.9), "Fri, 01 Jan 1971 01:01:01 GMT");
        let boundary = boundary();
        assert_eq!(boundary.len(), 26);
        assert!(boundary.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
