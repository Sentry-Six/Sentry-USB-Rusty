//! Serves TeslaCam MP4s with the index first.
//!
//! Tesla writes `[ftyp][free][mdat][moov]`: the index (`moov`) comes last, so
//! a player streaming a clip has to fetch the file's tail before it can show
//! a frame, and it opens a whole-file read alongside that competes for
//! bandwidth. This serves the same bytes as `[ftyp][moov][free][mdat]`, with
//! the index's chunk offsets shifted by the index's size. Nothing is
//! re-encoded or written: the video streams straight from disk, and only the
//! index (~18 KB per clip) is read and patched, cached by path, size and
//! mtime. Files without that shape fall through to the next service
//! unchanged.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use futures_util::stream::{self, BoxStream};
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

/// Largest index patched in memory; Tesla's are ~18 KB.
const MAX_MOOV_BYTES: u64 = 4 * 1024 * 1024;
/// Most top-level boxes a clip may have before it's passed through.
const MAX_TOP_LEVEL_BOXES: usize = 16;
/// Patched indexes kept (~18 KB each); the cache is cleared when full.
const CACHE_ENTRIES: usize = 256;
/// Boxes on the path from `moov` to the chunk offset tables.
const CONTAINERS: [&[u8; 4]; 5] = [b"moov", b"trak", b"mdia", b"minf", b"stbl"];

/// One file's reordered layout. Virtual bytes `[0, head_len)` are the file's
/// own, then the patched index, then the file's bytes from `head_len` up to
/// where the index started.
struct Layout {
    size: u64,
    mtime: SystemTime,
    /// End of `ftyp`, where the index moves to.
    head_len: u64,
    /// The patched index; in the file it runs from `size - moov.len()` to
    /// the end.
    moov: Bytes,
}

impl Layout {
    fn moov_len(&self) -> u64 {
        self.moov.len() as u64
    }

    /// The pieces making up virtual bytes `[start, end)`.
    fn segments(&self, start: u64, end: u64) -> Vec<Segment> {
        let moov_end = self.head_len + self.moov_len();
        let mut out = Vec::new();
        let mut piece = |from: u64, to: u64, make: &dyn Fn(u64, u64) -> Segment| {
            let (a, b) = (start.max(from), end.min(to));
            if a < b {
                out.push(make(a, b));
            }
        };
        piece(0, self.head_len, &|a, b| Segment::File { offset: a, len: b - a });
        piece(self.head_len, moov_end, &|a, b| Segment::Index {
            start: (a - self.head_len) as usize,
            end: (b - self.head_len) as usize,
        });
        let shift = self.moov_len();
        piece(moov_end, self.size, &|a, b| Segment::File { offset: a - shift, len: b - a });
        out
    }
}

#[derive(Debug, PartialEq)]
enum Segment {
    File { offset: u64, len: u64 },
    Index { start: usize, end: usize },
}

/// State for [`serve_index_first`]: the directory the URL paths map into.
pub struct IndexFirstMedia {
    root: PathBuf,
    cache: Mutex<HashMap<PathBuf, Arc<Layout>>>,
}

impl IndexFirstMedia {
    pub fn new(root: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self { root: root.into(), cache: Mutex::new(HashMap::new()) })
    }

    /// The file a request path names, for `.mp4` paths with only plain
    /// components (symlinks are followed, as ServeDir does).
    fn resolve(&self, uri_path: &str) -> Option<PathBuf> {
        let decoded = percent_decode(uri_path)?;
        if !decoded.to_ascii_lowercase().ends_with(".mp4") {
            return None;
        }
        let mut path = self.root.clone();
        for component in Path::new(&decoded).components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(part) => path.push(part),
                Component::ParentDir | Component::Prefix(_) => return None,
            }
        }
        Some(path)
    }

    async fn layout(&self, path: &Path) -> Option<Arc<Layout>> {
        let meta = tokio::fs::metadata(path).await.ok()?;
        if !meta.is_file() {
            return None;
        }
        let mtime = meta.modified().ok()?;
        if let Some(hit) = self.cache.lock().ok()?.get(path) {
            if hit.size == meta.len() && hit.mtime == mtime {
                return Some(hit.clone());
            }
        }
        let layout = Arc::new(read_layout(path, meta.len(), mtime).await?);
        let mut cache = self.cache.lock().ok()?;
        if cache.len() >= CACHE_ENTRIES {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), layout.clone());
        Some(layout)
    }
}

/// Middleware in front of the TeslaCam file service: answers GET and HEAD
/// for index-last MP4s itself, and passes everything else on.
pub async fn serve_index_first(
    State(media): State<Arc<IndexFirstMedia>>,
    req: Request,
    next: Next,
) -> Response {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return next.run(req).await;
    }
    let Some(path) = media.resolve(req.uri().path()) else {
        return next.run(req).await;
    };
    let Some(layout) = media.layout(&path).await else {
        return next.run(req).await;
    };
    match respond(&path, &layout, req.method() == Method::HEAD, req.headers()).await {
        Some(response) => response,
        None => next.run(req).await,
    }
}

async fn respond(path: &Path, layout: &Layout, head: bool, headers: &HeaderMap) -> Option<Response> {
    let etag = format!(
        "\"ix-{:x}-{:x}\"",
        layout.size,
        layout.mtime.duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    );
    let last_modified = chrono::DateTime::<chrono::Utc>::from(layout.mtime)
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    // A Range whose If-Range validator no longer matches gets the whole file.
    let if_range_ok = headers
        .get(header::IF_RANGE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v == etag || v == last_modified);
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .filter(|_| if_range_ok)
        .map_or(RangeRequest::Full, |v| parse_range(v, layout.size));

    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, "video/mp4")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, &etag)
        .header(header::LAST_MODIFIED, &last_modified);
    let (start, end) = match range {
        RangeRequest::Full => (0, layout.size),
        RangeRequest::Partial(start, end) => {
            builder = builder.status(StatusCode::PARTIAL_CONTENT).header(
                header::CONTENT_RANGE,
                format!("bytes {}-{}/{}", start, end - 1, layout.size),
            );
            (start, end)
        }
        RangeRequest::Unsatisfiable => {
            return builder
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{}", layout.size))
                .body(Body::empty())
                .ok();
        }
    };
    builder = builder.header(header::CONTENT_LENGTH, HeaderValue::from(end - start));
    if head {
        return builder.body(Body::empty()).ok();
    }

    let mut parts: Vec<BoxStream<'static, std::io::Result<Bytes>>> = Vec::new();
    for segment in layout.segments(start, end) {
        match segment {
            Segment::File { offset, len } => {
                let mut file = tokio::fs::File::open(path).await.ok()?;
                file.seek(SeekFrom::Start(offset)).await.ok()?;
                parts.push(ReaderStream::with_capacity(file.take(len), 64 * 1024).boxed());
            }
            Segment::Index { start, end } => {
                let bytes = layout.moov.slice(start..end);
                parts.push(stream::once(async move { Ok(bytes) }).boxed());
            }
        }
    }
    builder.body(Body::from_stream(stream::iter(parts).flatten())).ok()
}

#[derive(Debug, PartialEq)]
enum RangeRequest {
    Full,
    /// `[start, end)`.
    Partial(u64, u64),
    Unsatisfiable,
}

/// A single `bytes=` range; anything else (several ranges, other units,
/// malformed) is answered with the whole file, as RFC 9110 allows.
fn parse_range(value: &str, size: u64) -> RangeRequest {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return RangeRequest::Full;
    };
    if spec.contains(',') {
        return RangeRequest::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return RangeRequest::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        return match last.parse::<u64>() {
            Ok(0) => RangeRequest::Unsatisfiable,
            Ok(n) => RangeRequest::Partial(size.saturating_sub(n), size),
            Err(_) => RangeRequest::Full,
        };
    }
    let Ok(start) = first.parse::<u64>() else {
        return RangeRequest::Full;
    };
    if start >= size {
        return RangeRequest::Unsatisfiable;
    }
    let end = if last.is_empty() {
        size
    } else {
        match last.parse::<u64>() {
            Ok(last) if last >= start => (last + 1).min(size),
            _ => return RangeRequest::Full,
        }
    };
    RangeRequest::Partial(start, end)
}

/// Reads the top-level boxes and, for an index-last MP4, the patched index.
async fn read_layout(path: &Path, size: u64, mtime: SystemTime) -> Option<Layout> {
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let mut boxes = Vec::new();
    let mut offset = 0u64;
    while offset < size {
        if boxes.len() >= MAX_TOP_LEVEL_BOXES {
            return None;
        }
        let mut header = [0u8; 16];
        file.seek(SeekFrom::Start(offset)).await.ok()?;
        let n = read_up_to(&mut file, &mut header).await?;
        let (kind, _, box_size) = box_header(&header[..n], size - offset)?;
        boxes.push((kind, offset, box_size));
        offset += box_size;
    }
    let (head_len, moov_offset, moov_len) = plan(&boxes, size)?;
    let mut moov = vec![0u8; moov_len as usize];
    file.seek(SeekFrom::Start(moov_offset)).await.ok()?;
    file.read_exact(&mut moov).await.ok()?;
    shift_chunk_offsets(&mut moov, moov_len, head_len, moov_offset)?;
    Some(Layout { size, mtime, head_len, moov: Bytes::from(moov) })
}

async fn read_up_to(file: &mut tokio::fs::File, buf: &mut [u8]) -> Option<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]).await.ok()? {
            0 => break,
            n => filled += n,
        }
    }
    Some(filled)
}

/// `(type, header length, box size)` for the box starting at `buf`, where
/// `remaining` bytes are left in its parent.
fn box_header(buf: &[u8], remaining: u64) -> Option<([u8; 4], u64, u64)> {
    if buf.len() < 8 {
        return None;
    }
    let kind: [u8; 4] = buf[4..8].try_into().ok()?;
    let (header_len, size) = match u32::from_be_bytes(buf[..4].try_into().ok()?) {
        0 => (8, remaining),
        1 => (16, u64::from_be_bytes(buf.get(8..16)?.try_into().ok()?)),
        n => (8, u64::from(n)),
    };
    (size >= header_len && size <= remaining).then_some((kind, header_len, size))
}

/// `(head_len, moov_offset, moov_len)` when the file is `ftyp`, then boxes
/// including an `mdat`, then a final `moov` that runs to the end of the file.
fn plan(boxes: &[([u8; 4], u64, u64)], size: u64) -> Option<(u64, u64, u64)> {
    let (first, rest) = boxes.split_first()?;
    let (last, middle) = rest.split_last()?;
    if &first.0 != b"ftyp" || &last.0 != b"moov" || last.1 + last.2 != size {
        return None;
    }
    if !middle.iter().any(|b| &b.0 == b"mdat") || middle.iter().any(|b| &b.0 == b"moov") {
        return None;
    }
    if last.2 > MAX_MOOV_BYTES {
        return None;
    }
    Some((first.2, last.1, last.2))
}

/// Shifts every `stco`/`co64` entry in `moov` by `delta` (the index's own
/// size). Every entry must point between the end of `ftyp` and the index,
/// and a shifted `stco` entry must still fit 32 bits; otherwise None.
fn shift_chunk_offsets(moov: &mut [u8], delta: u64, head_len: u64, moov_offset: u64) -> Option<()> {
    let mut tables = 0usize;
    walk(moov, &mut |kind, body| {
        let width = match kind {
            b"stco" => 4,
            b"co64" => 8,
            _ => return Some(()),
        };
        tables += 1;
        let count = u32::from_be_bytes(body.get(4..8)?.try_into().ok()?) as usize;
        let entries = body.get_mut(8..8 + count.checked_mul(width)?)?;
        for entry in entries.chunks_exact_mut(width) {
            let old = if width == 4 {
                u64::from(u32::from_be_bytes(entry.try_into().ok()?))
            } else {
                u64::from_be_bytes(entry.try_into().ok()?)
            };
            if old < head_len || old >= moov_offset {
                return None;
            }
            let new = old.checked_add(delta)?;
            if width == 4 {
                entry.copy_from_slice(&u32::try_from(new).ok()?.to_be_bytes());
            } else {
                entry.copy_from_slice(&new.to_be_bytes());
            }
        }
        Some(())
    })?;
    (tables > 0).then_some(())
}

/// Visits each box in `buf`, descending into [`CONTAINERS`]; `visit` gets a
/// leaf's type and body.
#[allow(clippy::type_complexity)] // the visitor signature is clearest inline
fn walk(buf: &mut [u8], visit: &mut dyn FnMut(&[u8; 4], &mut [u8]) -> Option<()>) -> Option<()> {
    let mut offset = 0usize;
    while offset < buf.len() {
        let remaining = (buf.len() - offset) as u64;
        let (kind, header_len, size) = box_header(&buf[offset..], remaining)?;
        let body = &mut buf[offset + header_len as usize..offset + size as usize];
        if CONTAINERS.contains(&&kind) {
            walk(body, visit)?;
        } else {
            visit(&kind, body)?;
        }
        offset += size as usize;
    }
    Some(())
}

/// Decodes `%XX` escapes; None for malformed escapes or non-UTF-8 results.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok().filter(|s| !s.contains('\0'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn table(kind: &[u8; 4], offsets: &[u64]) -> Vec<u8> {
        let mut body = vec![0, 0, 0, 0];
        body.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
        for &o in offsets {
            if kind == b"stco" {
                body.extend_from_slice(&(o as u32).to_be_bytes());
            } else {
                body.extend_from_slice(&o.to_be_bytes());
            }
        }
        mp4_box(kind, &body)
    }

    /// `[ftyp][free][mdat][moov]` with two tracks whose chunk tables point at
    /// known payload bytes; returns the file and the chunk offsets.
    fn tesla_like() -> (Vec<u8>, Vec<u64>) {
        let ftyp = mp4_box(b"ftyp", b"mp42\0\0\0\0isommp42");
        let free = mp4_box(b"free", b"");
        let payload: Vec<u8> = (0..4000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mdat = mp4_box(b"mdat", &payload);
        let mdat_body = (ftyp.len() + free.len() + 8) as u64;
        let video = [mdat_body, mdat_body + 1000, mdat_body + 2500];
        let telemetry = [mdat_body + 3999];
        let stbl_v = mp4_box(b"stbl", &[mp4_box(b"stsd", b"avc1"), table(b"stco", &video)].concat());
        let stbl_t = mp4_box(b"stbl", &table(b"co64", &telemetry));
        let trak = |stbl: Vec<u8>| {
            mp4_box(b"trak", &mp4_box(b"mdia", &[mp4_box(b"mdhd", &[0; 24]), mp4_box(b"minf", &stbl)].concat()))
        };
        let moov = mp4_box(b"moov", &[mp4_box(b"mvhd", &[0; 100]), trak(stbl_v), trak(stbl_t)].concat());
        let file = [ftyp, free, mdat, moov].concat();
        (file, video.iter().chain(&telemetry).copied().collect())
    }

    fn top_level(file: &[u8]) -> Vec<([u8; 4], u64, u64)> {
        let mut out = Vec::new();
        let mut offset = 0u64;
        while (offset as usize) < file.len() {
            let (kind, _, size) = box_header(&file[offset as usize..], file.len() as u64 - offset).unwrap();
            out.push((kind, offset, size));
            offset += size;
        }
        out
    }

    fn layout_for(file: &[u8]) -> Option<Layout> {
        let size = file.len() as u64;
        let (head_len, moov_offset, moov_len) = plan(&top_level(file), size)?;
        let mut moov = file[moov_offset as usize..].to_vec();
        shift_chunk_offsets(&mut moov, moov_len, head_len, moov_offset)?;
        Some(Layout { size, mtime: UNIX_EPOCH, head_len, moov: Bytes::from(moov) })
    }

    fn virtual_bytes(file: &[u8], layout: &Layout, start: u64, end: u64) -> Vec<u8> {
        layout
            .segments(start, end)
            .into_iter()
            .flat_map(|segment| match segment {
                Segment::File { offset, len } => file[offset as usize..(offset + len) as usize].to_vec(),
                Segment::Index { start, end } => layout.moov[start..end].to_vec(),
            })
            .collect()
    }

    fn chunk_offsets(file: &[u8]) -> Vec<u64> {
        let boxes = top_level(file);
        let moov = boxes.iter().find(|b| &b.0 == b"moov").unwrap();
        let mut buf = file[moov.1 as usize..(moov.1 + moov.2) as usize].to_vec();
        let mut found = Vec::new();
        walk(&mut buf, &mut |kind, body| {
            let width = match kind {
                b"stco" => 4,
                b"co64" => 8,
                _ => return Some(()),
            };
            let count = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
            for entry in body[8..8 + count * width].chunks_exact(width) {
                found.push(if width == 4 {
                    u64::from(u32::from_be_bytes(entry.try_into().unwrap()))
                } else {
                    u64::from_be_bytes(entry.try_into().unwrap())
                });
            }
            Some(())
        })
        .unwrap();
        found
    }

    #[test]
    fn serves_the_index_first_with_offsets_still_pointing_at_the_same_bytes() {
        let (file, offsets) = tesla_like();
        let layout = layout_for(&file).expect("index-last file is reordered");
        let served = virtual_bytes(&file, &layout, 0, layout.size);
        assert_eq!(served.len(), file.len());
        let kinds: Vec<[u8; 4]> = top_level(&served).iter().map(|b| b.0).collect();
        assert_eq!(kinds, [*b"ftyp", *b"moov", *b"free", *b"mdat"]);
        let shifted = chunk_offsets(&served);
        assert_eq!(shifted.len(), offsets.len());
        for (old, new) in offsets.iter().zip(&shifted) {
            assert_eq!(*new, old + layout.moov_len());
            assert_eq!(served[*new as usize], file[*old as usize]);
        }
    }

    #[test]
    fn any_range_matches_the_same_slice_of_the_whole_response() {
        let (file, _) = tesla_like();
        let layout = layout_for(&file).unwrap();
        let whole = virtual_bytes(&file, &layout, 0, layout.size);
        let moov_end = layout.head_len + layout.moov_len();
        let edges = [0, 1, layout.head_len - 1, layout.head_len, layout.head_len + 1, moov_end - 1, moov_end, moov_end + 1, layout.size - 1, layout.size];
        for &start in &edges {
            for &end in &edges {
                if start < end {
                    assert_eq!(virtual_bytes(&file, &layout, start, end), whole[start as usize..end as usize], "{start}..{end}");
                }
            }
        }
    }

    #[test]
    fn leaves_other_layouts_alone() {
        let (file, _) = tesla_like();
        let boxes = top_level(&file);
        // Already index-first.
        let reordered = [boxes[0], boxes[3], boxes[1], boxes[2]];
        assert!(plan(&reordered, file.len() as u64).is_none());
        // Something after the index.
        let mut trailing = boxes.clone();
        trailing.push((*b"free", file.len() as u64, 8));
        assert!(plan(&trailing, file.len() as u64 + 8).is_none());
        // No video data.
        assert!(plan(&[boxes[0], boxes[1], boxes[3]], file.len() as u64).is_none());
    }

    #[test]
    fn refuses_offsets_outside_the_video_data_or_past_32_bits() {
        let mut moov = mp4_box(b"moov", &mp4_box(b"trak", &mp4_box(b"mdia", &mp4_box(b"minf", &mp4_box(b"stbl", &table(b"stco", &[10]))))));
        assert!(shift_chunk_offsets(&mut moov.clone(), 100, 40, 1000).is_none(), "points into ftyp");
        assert!(shift_chunk_offsets(&mut moov, u64::from(u32::MAX), 0, 1000).is_none(), "overflows stco");
        let mut no_tables = mp4_box(b"moov", &mp4_box(b"mvhd", &[0; 100]));
        assert!(shift_chunk_offsets(&mut no_tables, 100, 0, 1000).is_none());
    }

    #[test]
    fn parses_single_byte_ranges() {
        assert_eq!(parse_range("bytes=0-1", 100), RangeRequest::Partial(0, 2));
        assert_eq!(parse_range("bytes=90-", 100), RangeRequest::Partial(90, 100));
        assert_eq!(parse_range("bytes=-10", 100), RangeRequest::Partial(90, 100));
        assert_eq!(parse_range("bytes=50-500", 100), RangeRequest::Partial(50, 100));
        assert_eq!(parse_range("bytes=100-", 100), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", 100), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range("bytes=0-1,5-6", 100), RangeRequest::Full);
        assert_eq!(parse_range("bytes=5-1", 100), RangeRequest::Full);
        assert_eq!(parse_range("items=0-1", 100), RangeRequest::Full);
    }

    #[test]
    fn resolves_only_plain_mp4_paths_under_the_root() {
        let media = IndexFirstMedia::new("/srv/cam");
        assert_eq!(
            media.resolve("/SavedClips/2026-10-22_19-00-00/2026-10-22_18-59-00-front.mp4"),
            Some(PathBuf::from("/srv/cam/SavedClips/2026-10-22_19-00-00/2026-10-22_18-59-00-front.mp4"))
        );
        assert_eq!(media.resolve("/a%20b/c.MP4"), Some(PathBuf::from("/srv/cam/a b/c.MP4")));
        assert!(media.resolve("/../etc/x.mp4").is_none());
        assert!(media.resolve("/a/%2e%2e/x.mp4").is_none());
        assert!(media.resolve("/event.json").is_none());
        assert!(media.resolve("/bad%zz.mp4").is_none());
    }

    #[tokio::test]
    async fn sits_in_front_of_the_file_service() {
        use tower::ServiceExt;
        let (file, _) = tesla_like();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("SavedClips/ev")).unwrap();
        std::fs::write(dir.path().join("SavedClips/ev/clip-front.mp4"), &file).unwrap();
        std::fs::write(dir.path().join("SavedClips/ev/event.json"), b"{}").unwrap();
        let app = axum::Router::new()
            .fallback(|| async { (StatusCode::IM_A_TEAPOT, "file service") })
            .layer(axum::middleware::from_fn_with_state(IndexFirstMedia::new(dir.path()), serve_index_first));
        let get = |uri: &str, range: Option<&str>| {
            let mut req = Request::builder().uri(uri);
            if let Some(range) = range {
                req = req.header(header::RANGE, range);
            }
            app.clone().oneshot(req.body(Body::empty()).unwrap())
        };

        let response = get("/SavedClips/ev/clip-front.mp4", Some("bytes=0-63")).await.unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[4..8], b"ftyp");
        let ftyp_len = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
        assert_eq!(&body[ftyp_len + 4..ftyp_len + 8], b"moov", "index follows ftyp");

        for passthrough in ["/SavedClips/ev/event.json", "/SavedClips/ev/missing.mp4"] {
            assert_eq!(get(passthrough, None).await.unwrap().status(), StatusCode::IM_A_TEAPOT, "{passthrough}");
        }
        let post = Request::builder().method(Method::POST).uri("/SavedClips/ev/clip-front.mp4").body(Body::empty()).unwrap();
        assert_eq!(app.clone().oneshot(post).await.unwrap().status(), StatusCode::IM_A_TEAPOT);
    }

    #[tokio::test]
    async fn answers_ranges_from_disk() {
        let (file, _) = tesla_like();
        let dir = std::env::temp_dir().join(format!("ix-first-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip-front.mp4");
        std::fs::write(&path, &file).unwrap();
        let media = IndexFirstMedia::new(&dir);
        let layout = media.layout(&path).await.expect("layout");
        let whole = virtual_bytes(&file, &layout, 0, layout.size);

        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_static("bytes=30-199"));
        let response = respond(&path, &layout, false, &headers).await.unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE], format!("bytes 30-199/{}", file.len()));
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], &whole[30..200]);

        let response = respond(&path, &layout, false, &HeaderMap::new()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], &whole[..]);

        headers.insert(header::IF_RANGE, HeaderValue::from_static("\"stale\""));
        let response = respond(&path, &layout, true, &headers).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "stale If-Range gets the whole file");
        assert_eq!(response.headers()[header::CONTENT_LENGTH], file.len().to_string());

        std::fs::remove_dir_all(&dir).ok();
    }
}
