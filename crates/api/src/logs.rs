//! Log file viewer.
//!
//! Returns raw `text/plain` content (the frontend parses the text directly).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::io::{Read, Seek, SeekFrom};

use crate::router::AppState;

/// Known log files and their paths.
fn log_path(name: &str) -> Option<&'static str> {
    match name {
        "archiveloop" => Some("/mutable/archiveloop.log"),
        "setup" => Some("/sentryusb/sentryusb-setup.log"),
        "diagnostics" => Some("/tmp/diagnostics.txt"),
        "syslog" => Some("/var/log/syslog"),
        "kern" => Some("/var/log/kern.log"),
        "auth" => Some("/var/log/auth.log"),
        "daemon" => Some("/var/log/daemon.log"),
        "sentryusb" => Some("/var/log/sentryusb.log"),
        "sentryusb-ble" => Some("/var/log/sentryusb-ble.log"),
        _ => None,
    }
}

/// Bound each response for low-memory devices and unrotated logs.
const MAX_TAIL_BYTES: u64 = 512 * 1024;

/// GET /api/logs/{name}
///
/// Returns the tail of the log file as `text/plain`.
pub async fn get_log(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    // Bluetooth diagnostics are generated live rather than read from a file.
    if name == "bluetooth" {
        return crate::ble_debug::get_ble_debug(State(s)).await;
    }
    get_log_tail(Path(name)).await
}

/// State-free tail read — also mounted by the degraded router, where no
/// AppState exists (bluetooth's live dump needs the full app, so only
/// the plain-file path is available there).
pub async fn get_log_tail(Path(name): Path<String>) -> Response {
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return (StatusCode::BAD_REQUEST, "invalid log name").into_response();
    }

    // Keep bounded SD-card reads off the async reactor.
    tokio::task::spawn_blocking(move || read_log_tail(name))
        .await
        .unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "log read task failed").into_response()
        })
}

fn read_log_tail(name: String) -> Response {
    let known = log_path(&name).is_some();
    let path = match log_path(&name) {
        Some(p) => p.to_string(),
        None => format!("/var/log/{}", name),
    };

    let mut file = match std::fs::File::open(&path) {
        Ok(f) => f,
        // Known logs may not exist yet; unknown names still return 404.
        Err(_) if known => {
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                String::new(),
            ).into_response();
        }
        Err(_) => return (StatusCode::NOT_FOUND, "Log file not found").into_response(),
    };

    let meta = match file.metadata() {
        Ok(m) => m,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot stat log file").into_response(),
    };

    // If the file is larger than the cap, seek to the last MAX_TAIL_BYTES and
    // skip the first partial line so output starts at a clean boundary.
    if meta.len() > MAX_TAIL_BYTES {
        let _ = file.seek(SeekFrom::End(-(MAX_TAIL_BYTES as i64)));
        let mut one = [0u8; 1];
        loop {
            match file.read(&mut one) {
                Ok(1) if one[0] == b'\n' => break,
                Ok(1) => continue,
                _ => break,
            }
        }
    }

    let mut buf = String::new();
    if let Err(_) = file.read_to_string(&mut buf) {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to read log file").into_response();
    }

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        buf,
    )
        .into_response()
}

/// Default and maximum page sizes, bounded for BLE transport latency.
const DEFAULT_PAGE_LINES: usize = 50;
const MAX_PAGE_LINES: usize = 2000;

#[derive(Deserialize)]
pub struct LogPageQuery {
    lines: Option<usize>,
    /// Byte offset returned by a previous page. Reads the lines immediately
    /// before it; absent means start at the end of the file.
    before: Option<u64>,
    /// Optional identity from the tail endpoint; rejects rotation while paging.
    cursor: Option<String>,
}

/// GET /api/logs/{name}/page?lines=50&before=<offset>
///
/// JSON sibling of `get_log` for incremental older-line scrolling.
/// The text endpoint remains compatible, and BLE-safe cursors travel in JSON.
pub async fn get_log_page(
    Path(name): Path<String>,
    Query(q): Query<LogPageQuery>,
) -> Response {
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return page_error(StatusCode::BAD_REQUEST, "invalid log name");
    }

    // Live diagnostics have no stable byte offsets.
    if name == "bluetooth" {
        return page_error(
            StatusCode::BAD_REQUEST,
            "the bluetooth log is generated on demand and cannot be paged",
        );
    }

    let lines = q.lines.unwrap_or(DEFAULT_PAGE_LINES).clamp(1, MAX_PAGE_LINES);
    let before = q.before;

    tokio::task::spawn_blocking(move || read_log_page(name, lines, before, q.cursor))
        .await
        .unwrap_or_else(|_| {
            page_error(StatusCode::INTERNAL_SERVER_ERROR, "log read task failed")
        })
}

fn page_error(status: StatusCode, msg: &str) -> Response {
    crate::json_error(status, msg).into_response()
}

fn page_response(content: String, start: u64) -> Response {
    let has_more = start > 0;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "content": content,
            // Feed this cursor back as `before`.
            "before": if has_more { Some(start) } else { None },
            "has_more": has_more,
        })),
    )
        .into_response()
}

fn read_log_page(name: String, lines: usize, before: Option<u64>, cursor: Option<String>) -> Response {
    let known = log_path(&name).is_some();
    let path = match log_path(&name) {
        Some(p) => p.to_string(),
        None => format!("/var/log/{}", name),
    };

    let mut file = match std::fs::File::open(&path) {
        Ok(f) => f,
        // A known log that has not been created yet is an empty page.
        Err(_) if cursor.is_some() => return page_error(StatusCode::CONFLICT, "Log rotated. Reload its newest entries."),
        Err(e) if known && e.kind() == std::io::ErrorKind::NotFound => return page_response(String::new(), 0),
        Err(_) => return page_error(StatusCode::NOT_FOUND, "Log file not found"),
    };

    match guarded_page_window(&mut file, lines, before, cursor.as_deref()) {
        Ok((content, start)) => page_response(content, start),
        Err(PageError::StaleCursor) => page_error(
            StatusCode::CONFLICT,
            "log rotated since that page; reload from the newest page",
        ),
        Err(PageError::Io) => {
            page_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read log file")
        }
    }
}

#[derive(Debug)]
enum PageError {
    /// `before` points past EOF — the log rotated or was truncated.
    StaleCursor,
    Io,
}

/// Returns the last `lines` lines ending at `before` (default EOF), plus the
/// byte offset the window starts at. Split out from the handler so the
/// backwards line walk is unit-testable.
fn page_window(
    file: &mut std::fs::File,
    lines: usize,
    before: Option<u64>,
) -> Result<(String, u64), PageError> {
    let len = file.metadata().map_err(|_| PageError::Io)?.len();

    let end = before.unwrap_or(len);
    if end > len {
        return Err(PageError::StaleCursor);
    }
    if end == 0 {
        return Ok((String::new(), 0));
    }

    // A trailing newline terminates the final line; it does not add one.
    let mut scan_end = end;
    let mut one = [0u8; 1];
    if file.seek(SeekFrom::Start(end - 1)).is_ok()
        && file.read_exact(&mut one).is_ok()
        && one[0] == b'\n'
    {
        scan_end -= 1;
    }

    const CHUNK: u64 = 8 * 1024;
    let mut buf = vec![0u8; CHUNK as usize];
    let mut start = scan_end;
    let mut found = 0usize;

    'outer: while start > 0 && end - start < MAX_TAIL_BYTES {
        let step = CHUNK.min(start);
        let pos = start - step;
        if file.seek(SeekFrom::Start(pos)).is_err() {
            break;
        }
        let slice = &mut buf[..step as usize];
        if file.read_exact(slice).is_err() {
            break;
        }
        for i in (0..step as usize).rev() {
            if slice[i] == b'\n' {
                found += 1;
                if found == lines {
                    start = pos + i as u64 + 1;
                    break 'outer;
                }
            }
        }
        start = pos;
    }

    // Return a bounded fragment if one page exceeds the byte cap.
    if end - start > MAX_TAIL_BYTES {
        start = end - MAX_TAIL_BYTES;
    }

    let take = (end - start) as usize;
    let mut out = vec![0u8; take];
    file.seek(SeekFrom::Start(start)).map_err(|_| PageError::Io)?;
    file.read_exact(&mut out).map_err(|_| PageError::Io)?;

    // A byte-capped long line can start inside a codepoint.
    let skip = if start > 0 { out.iter().take_while(|byte| **byte & 0xc0 == 0x80).count() } else { 0 };
    Ok((String::from_utf8_lossy(&out[skip..]).into_owned(), start + skip as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(body: &str) -> std::fs::File {
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f
    }

    #[test]
    fn returns_last_n_lines_with_trailing_newline() {
        let mut f = fixture("a\nb\nc\nd\n");
        let (content, start) = page_window(&mut f, 2, None).unwrap();
        assert_eq!(content, "c\nd\n");
        assert_eq!(start, 4);
    }

    #[test]
    fn returns_last_n_lines_without_trailing_newline() {
        let mut f = fixture("a\nb\nc\nd");
        let (content, start) = page_window(&mut f, 2, None).unwrap();
        assert_eq!(content, "c\nd");
        assert_eq!(start, 4);
    }

    #[test]
    fn walks_backwards_through_pages_to_the_start() {
        let mut f = fixture("a\nb\nc\nd\n");
        let (_, first) = page_window(&mut f, 2, None).unwrap();
        let (content, start) = page_window(&mut f, 2, Some(first)).unwrap();
        assert_eq!(content, "a\nb\n");
        assert_eq!(start, 0);
    }

    #[test]
    fn asking_for_more_lines_than_exist_returns_the_whole_file() {
        let mut f = fixture("a\nb\n");
        let (content, start) = page_window(&mut f, 500, None).unwrap();
        assert_eq!(content, "a\nb\n");
        assert_eq!(start, 0);
    }

    #[test]
    fn empty_file_is_an_empty_page() {
        let mut f = fixture("");
        let (content, start) = page_window(&mut f, 50, None).unwrap();
        assert!(content.is_empty());
        assert_eq!(start, 0);
    }

    #[test]
    fn spans_the_chunk_boundary() {
        let body: String = (0..4000).map(|i| format!("line {i}\n")).collect();
        let mut f = fixture(&body);
        let (content, _) = page_window(&mut f, 3, None).unwrap();
        assert_eq!(content, "line 3997\nline 3998\nline 3999\n");
    }

    #[test]
    fn cursor_past_eof_is_rejected() {
        let mut f = fixture("a\nb\n");
        assert!(matches!(
            page_window(&mut f, 2, Some(9_999)),
            Err(PageError::StaleCursor)
        ));
    }
}

#[derive(Deserialize)]
pub struct LogDeltaQuery {
    cursor: Option<String>,
}

#[derive(serde::Serialize)]
struct LogDelta {
    content: String,
    cursor: String,
    before: u64,
    reset: bool,
    has_more: bool,
}

/// Bounded append-only reads. The cursor binds the inode and preceding bytes,
/// so rotation and copy-truncate trigger a fresh tail instead of mixing files.
pub async fn get_log_delta(Path(name): Path<String>, Query(query): Query<LogDeltaQuery>) -> Response {
    if name.contains("..") || name.contains('/') || name.contains('\\') || name == "bluetooth" {
        return page_error(StatusCode::BAD_REQUEST, "invalid log name");
    }
    tokio::task::spawn_blocking(move || {
        let path = log_path(&name).map(str::to_owned).unwrap_or_else(|| format!("/var/log/{name}"));
        match std::fs::File::open(path) {
            Ok(mut file) => match delta_window(&mut file, query.cursor.as_deref()) {
                Ok(delta) => ([(header::CACHE_CONTROL, "no-store")], Json(delta)).into_response(),
                Err(_) => page_error(StatusCode::INTERNAL_SERVER_ERROR, "Cannot read log"),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && log_path(&name).is_some() => {
                ([(header::CACHE_CONTROL, "no-store")], Json(LogDelta { content: String::new(), cursor: String::new(), before: 0, reset: true, has_more: false })).into_response()
            }
            Err(_) => page_error(StatusCode::NOT_FOUND, "Log file unavailable"),
        }
    }).await.unwrap_or_else(|_| page_error(StatusCode::INTERNAL_SERVER_ERROR, "Log read failed"))
}

fn cursor_at(file: &mut std::fs::File, position: u64) -> std::io::Result<String> {
    use std::hash::{Hash, Hasher};
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    let start = position.saturating_sub(64);
    let mut preceding = vec![0; (position - start) as usize];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut preceding)?;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    preceding.hash(&mut hash);
    Ok(format!("{}:{}:{position}:{}", metadata.dev(), metadata.ino(), hash.finish()))
}

fn cursor_position(file: &mut std::fs::File, cursor: &str) -> std::io::Result<Option<u64>> {
    let position = cursor.split(':').nth(2).and_then(|part| part.parse::<u64>().ok());
    match position {
        Some(at) if at <= file.metadata()?.len() && cursor_at(file, at)? == cursor => Ok(Some(at)),
        _ => Ok(None),
    }
}

fn guarded_page_window(file: &mut std::fs::File, lines: usize, before: Option<u64>, cursor: Option<&str>) -> Result<(String, u64), PageError> {
    if let Some(cursor) = cursor {
        if cursor_position(file, cursor).map_err(|_| PageError::Io)?.is_none() {
            return Err(PageError::StaleCursor);
        }
    }
    page_window(file, lines, before)
}

// Hold back only an unfinished trailing codepoint, even after malformed bytes.
fn complete_utf8_len(bytes: &[u8]) -> usize {
    let mut offset = 0;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(_) => return bytes.len(),
            Err(error) => match error.error_len() {
                Some(length) => offset += error.valid_up_to() + length,
                None => return offset + error.valid_up_to(),
            },
        }
    }
    bytes.len()
}

fn complete_log_end(file: &mut std::fs::File, length: u64) -> std::io::Result<u64> {
    let start = length.saturating_sub(4);
    let mut suffix = vec![0; (length - start) as usize];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut suffix)?;
    Ok(start + complete_utf8_len(&suffix) as u64)
}

fn delta_window(file: &mut std::fs::File, cursor: Option<&str>) -> std::io::Result<LogDelta> {
    let length = file.metadata()?.len();
    let requested = match cursor { Some(cursor) => cursor_position(file, cursor)?, None => None };
    let complete_end = complete_log_end(file, length)?;
    let Some(start) = requested else {
        let (content, before) = page_window(file, 500, Some(complete_end))
            .map_err(|_| std::io::Error::other("Unable to read initial tail"))?;
        return Ok(LogDelta { content, cursor: cursor_at(file, complete_end)?, before, reset: true, has_more: false });
    };
    let mut bytes = vec![0; (complete_end.saturating_sub(start)).min(64 * 1024) as usize];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut bytes)?;
    bytes.truncate(complete_utf8_len(&bytes));
    let end = start + bytes.len() as u64;
    Ok(LogDelta { content: String::from_utf8_lossy(&bytes).into_owned(), cursor: cursor_at(file, end)?, before: start, reset: false, has_more: end < complete_end })
}

#[cfg(test)]
mod delta_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn unchanged_logs_send_nothing_and_appends_send_only_new_content() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"first\n").unwrap();
        let initial = delta_window(&mut file, None).unwrap();
        assert!(initial.reset);
        assert!(delta_window(&mut file, Some(&initial.cursor)).unwrap().content.is_empty());
        file.seek(SeekFrom::End(0)).unwrap(); file.write_all(b"second\n").unwrap();
        let next = delta_window(&mut file, Some(&initial.cursor)).unwrap();
        assert!(!next.reset); assert_eq!(next.content, "second\n");
    }

    #[test]
    fn copy_truncate_and_replacement_are_detected() {
        let mut file = tempfile::tempfile().unwrap(); file.write_all(b"old log\n").unwrap();
        let initial = delta_window(&mut file, None).unwrap();
        file.set_len(0).unwrap(); file.seek(SeekFrom::Start(0)).unwrap(); file.write_all(b"new log longer\n").unwrap();
        let next = delta_window(&mut file, Some(&initial.cursor)).unwrap();
        assert!(next.reset); assert_eq!(next.content, "new log longer\n");
        let mut other = tempfile::tempfile().unwrap(); other.write_all(b"new log longer\n").unwrap();
        assert!(delta_window(&mut other, Some(&next.cursor)).unwrap().reset);
    }
    #[test]
    fn incomplete_unicode_waits_without_fast_polling_and_completes_losslessly() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"first\n\xf0\x9f").unwrap();
        let initial = delta_window(&mut file, None).unwrap();
        assert_eq!(initial.content, "first\n");
        let unchanged = delta_window(&mut file, Some(&initial.cursor)).unwrap();
        assert_eq!(unchanged.content, "");
        assert!(!unchanged.has_more);
        assert_eq!(unchanged.cursor, initial.cursor);
        file.seek(SeekFrom::End(0)).unwrap();
        file.write_all(b"\x9a\x97\n").unwrap();
        let completed = delta_window(&mut file, Some(&initial.cursor)).unwrap();
        assert_eq!(completed.content, "🚗\n");
        assert!(!completed.has_more);
    }

    #[test]
    fn older_pages_reject_same_size_replacement_and_allow_appends() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"one\ntwo\nthree\n").unwrap();
        let original = delta_window(&mut file, None).unwrap();
        file.seek(SeekFrom::End(0)).unwrap(); file.write_all(b"four\n").unwrap();
        assert_eq!(guarded_page_window(&mut file, 1, Some(4), Some(&original.cursor)).unwrap().0, "one\n");
        file.set_len(0).unwrap(); file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"new\nlog\nmore!\nlonger\n").unwrap();
        assert!(matches!(guarded_page_window(&mut file, 1, Some(4), Some(&original.cursor)), Err(PageError::StaleCursor)));
    }

    #[test]
    fn byte_capped_initial_tail_starts_on_unicode_boundary() {
        let body = "🚗".repeat(140_000) + "a";
        let mut file = tempfile::tempfile().unwrap(); file.write_all(body.as_bytes()).unwrap();
        let initial = delta_window(&mut file, None).unwrap();
        assert!(!initial.content.contains('�'));
        assert_eq!(initial.before as usize + initial.content.len(), body.len());
    }

}
