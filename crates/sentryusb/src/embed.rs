use std::fmt::Write;

use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::{Embed, EmbeddedFile};

#[derive(Embed)]
#[folder = "static/"]
struct StaticFiles;

/// Hand-rolled MIME table. The full `mime_guess` crate ships a
/// thousands-entry generated database we don't need — we serve a
/// closed set of extensions out of the embedded SPA bundle plus its
/// pre-compressed siblings. Fallback is `application/octet-stream`,
/// which is the same default `mime_guess::first_or_octet_stream()`
/// would have returned.
fn mime_for(path: &str) -> &'static str {
    // For the pre-compressed siblings the MIME of the *original*
    // resource is what we advertise; Content-Encoding handles the
    // wrapping. `foo.js.br` → `application/javascript`.
    let stem = path
        .strip_suffix(".br")
        .or_else(|| path.strip_suffix(".gz"))
        .unwrap_or(path);
    let ext = stem.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "gif" => "image/gif",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// SPA fallback handler: serve static files or index.html for client-side
/// routing. Sets caching headers so repeat page loads don't re-download the
/// JS bundle from a car parked outside on flaky WiFi:
///
///   /assets/* — Vite content-hashes these (e.g. `index-CThdLhPi.js`); the
///   bytes are immutable, so cache them forever.
///
///   index.html and other entry files — `no-cache` so a soft reload picks
///   up a new bundle after an OTA update without a hard refresh.
///
/// If `build.sh` produced `.br` / `.gz` siblings for the asset, we serve
/// the pre-compressed bytes directly — no per-request compression CPU,
/// which matters on the Pi Zero 2W. Browsers that don't advertise br/gzip
/// support fall back to the raw bytes.
///
/// ETag is the first 16 bytes (hex) of the sha256 hash that rust-embed
/// pre-computes at compile time, suffixed with the encoding so a client
/// that downgrades from br→identity gets a fresh body instead of a stale
/// 304. If the client's `If-None-Match` matches, return 304 — the asset
/// isn't re-sent.
pub async fn spa_handler(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');

    let path = if StaticFiles::get(path).is_some() { path } else { "index.html" };
    match pick_encoding(path, &headers) {
        Some((file, encoding)) => serve_embedded(path, file, encoding, &headers),
        None if StaticFiles::get(path).is_some() => {
            (StatusCode::NOT_ACCEPTABLE, "No acceptable content encoding").into_response()
        }
        None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

fn preferred_encodings(accept: &str) -> Vec<&'static str> {
    let mut explicit = std::collections::HashMap::new();
    for item in accept.split(',') {
        let mut fields = item.trim().split(';');
        let coding = fields.next().unwrap_or("").trim().to_ascii_lowercase();
        let mut quality = 1.0_f32;
        for field in fields {
            if let Some((name, value)) = field.trim().split_once('=') {
                if name.trim().eq_ignore_ascii_case("q") {
                    quality = value.trim().parse::<f32>().ok()
                        .filter(|q| q.is_finite() && (0.0..=1.0).contains(q)).unwrap_or(0.0);
                }
            }
        }
        explicit.insert(coding, quality);
    }
    let wildcard = explicit.get("*").copied();
    let mut choices: Vec<(&str, f32)> = ["br", "gzip"].into_iter()
        .map(|coding| (coding, explicit.get(coding).copied().or(wildcard).unwrap_or(0.0)))
        .filter(|(_, q)| *q > 0.0).collect();
    if let Some(q) = explicit.get("identity").copied() {
        if q > 0.0 { choices.push(("identity", q)); }
    }
    choices.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut result: Vec<_> = choices.into_iter().map(|(coding, _)| coding).collect();
    // Identity is the fallback unless the client explicitly excludes it.
    if !explicit.contains_key("identity") && wildcard != Some(0.0) {
        result.push("identity");
    }
    result
}

fn pick_encoding(path: &str, req_headers: &HeaderMap) -> Option<(EmbeddedFile, Option<&'static str>)> {
    let accept = req_headers.get_all(header::ACCEPT_ENCODING).iter()
        .filter_map(|v| v.to_str().ok()).collect::<Vec<_>>().join(",");
    for encoding in preferred_encodings(&accept) {
        let (filename, header) = match encoding {
            "br" => (format!("{path}.br"), Some("br")),
            "gzip" => (format!("{path}.gz"), Some("gzip")),
            _ => (path.to_owned(), None),
        };
        if let Some(file) = StaticFiles::get(&filename) { return Some((file, header)); }
    }
    None
}

fn serve_embedded(
    path: &str,
    file: EmbeddedFile,
    encoding: Option<&'static str>,
    req_headers: &HeaderMap,
) -> Response {
    let etag = etag_for(&file, encoding);
    let cache_control = cache_control_for(path);

    if let Some(if_none_match) = req_headers.get(header::IF_NONE_MATCH) {
        if if_none_match.as_bytes() == etag.as_bytes() {
            let mut resp = Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .header(header::CACHE_CONTROL, cache_control)
                .header(header::VARY, "Accept-Encoding")
                .header(header::ETAG, &etag);
            if let Some(enc) = encoding {
                resp = resp.header(header::CONTENT_ENCODING, enc);
            }
            return resp.body(axum::body::Body::empty()).unwrap();
        }
    }

    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime_for(path))
        .header(header::CACHE_CONTROL, cache_control)
        .header(header::VARY, "Accept-Encoding")
        .header(header::ETAG, &etag);
    if let Some(enc) = encoding {
        // Tell intermediaries the encoded body varies by Accept-Encoding
        // so a proxy doesn't hand the br bytes to a client that asked
        // for identity.
        resp = resp.header(header::CONTENT_ENCODING, enc);
        resp = resp.header(header::VARY, "Accept-Encoding");
    }
    resp.body(axum::body::Body::from(file.data)).unwrap()
}

fn etag_for(file: &EmbeddedFile, encoding: Option<&str>) -> String {
    let hash = file.metadata.sha256_hash();
    let mut s = String::with_capacity(40);
    s.push('"');
    for b in &hash[..16] {
        let _ = write!(s, "{:02x}", b);
    }
    if let Some(enc) = encoding {
        s.push('-');
        s.push_str(enc);
    }
    s.push('"');
    s
}

fn cache_control_for(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

#[cfg(test)]
mod tests {
    use super::preferred_encodings;

    #[test]
    fn honors_quality_exclusions_and_identity() {
        assert_eq!(preferred_encodings("br;q=0, gzip"), vec!["gzip", "identity"]);
        assert_eq!(preferred_encodings("br;q=0.3, gzip;q=0.8"), vec!["gzip", "br", "identity"]);
        assert_eq!(preferred_encodings("gzip;q=0.5, identity;q=1"), vec!["identity", "gzip"]);
        assert_eq!(preferred_encodings("*;q=0"), Vec::<&str>::new());
        assert_eq!(preferred_encodings("*;q=1, br;q=0"), vec!["gzip", "identity"]);
        assert_eq!(preferred_encodings("x-br, gzip;q=NaN"), vec!["identity"]);
        assert_eq!(preferred_encodings(""), vec!["identity"]);
    }
}
