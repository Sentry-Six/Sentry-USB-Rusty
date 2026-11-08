//! Fetches release files from GitHub with retries and readable failures.
//!
//! GitHub's release-download links (github.com/<repo>/releases/download/…)
//! can answer 504 or stall without a byte for minutes while its status page
//! reports no incident, as on 2026-11-08. Each file is therefore tried a few
//! times, from the download link and from the REST API's asset endpoint, which
//! is served separately, and a failure names its cause instead of claiming the
//! release has no binary.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

const USER_AGENT: &str = concat!("sentryusb-updater/", env!("CARGO_PKG_VERSION"));
/// Pauses before each retry round; the first round starts immediately.
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(3), Duration::from_secs(8)];
/// Probes and API lookups are small, so a slow answer means GitHub is stuck.
const SMALL_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// A download that sends nothing for this long has stalled.
const STALL_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;

fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(STALL_TIMEOUT)
            // Backstop only: a 20 MB binary over a weak hotspot can take minutes.
            .timeout(Duration::from_secs(900))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// Why a request to GitHub failed, in terms a user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// GitHub answered that the release or file does not exist.
    NotFound,
    /// GitHub answered with another HTTP error.
    Http(u16),
    RateLimited,
    Timeout,
    Dns,
    Connect(String),
    Tls(String),
    /// The file arrived truncated or did not match GitHub's checksum.
    Integrity(String),
    /// Writing the file on the Pi failed; retrying will not help.
    Local(String),
    Other(String),
}

impl FetchError {
    fn retryable(&self) -> bool {
        !matches!(self, FetchError::NotFound | FetchError::Local(_))
    }

    /// One plain clause describing what happened.
    pub fn reason(&self) -> String {
        match self {
            FetchError::NotFound => "GitHub has no such file".into(),
            FetchError::Http(s) if *s >= 500 => format!("GitHub's servers returned an error (HTTP {s})"),
            FetchError::Http(s) => format!("GitHub refused the request (HTTP {s})"),
            FetchError::RateLimited => "GitHub's download limit for this network was reached".into(),
            FetchError::Timeout => "GitHub didn't respond in time".into(),
            FetchError::Dns => "the Pi couldn't look up github.com".into(),
            FetchError::Connect(d) => format!("the Pi couldn't connect to GitHub ({d})"),
            FetchError::Tls(d) => format!("the secure connection to GitHub failed ({d})"),
            FetchError::Integrity(d) | FetchError::Local(d) => d.clone(),
            FetchError::Other(d) => format!("the download failed ({d})"),
        }
    }

    /// What the user can do about it; empty when there is nothing to suggest.
    pub fn advice(&self) -> &'static str {
        match self {
            FetchError::Http(s) if *s >= 500 => {
                "This is a problem on GitHub's side, not your Pi. Try again in a few minutes."
            }
            FetchError::Timeout => {
                "GitHub may be having problems, or the Pi's internet connection is weak. Try again in a few minutes."
            }
            FetchError::Integrity(_) | FetchError::Other(_) => {
                "This is usually a brief GitHub or network problem. Try again in a few minutes."
            }
            FetchError::RateLimited => "Wait about an hour and try again.",
            FetchError::Dns | FetchError::Connect(_) => {
                "Check that the Pi is connected to Wi-Fi with internet access, then try again."
            }
            FetchError::Tls(_) => "Check that the Pi's date and time are correct, then try again.",
            FetchError::NotFound | FetchError::Http(_) | FetchError::Local(_) => "",
        }
    }

    /// "Reason. Advice" as one message.
    pub fn sentence(&self) -> String {
        let mut reason = self.reason();
        if let Some(first) = reason.get(..1) {
            reason = first.to_uppercase() + &reason[1..];
        }
        match self.advice() {
            "" => format!("{reason}."),
            advice => format!("{reason}. {advice}"),
        }
    }
}

fn status_error(status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap) -> FetchError {
    let exhausted = headers
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "0");
    match status.as_u16() {
        404 => FetchError::NotFound,
        429 => FetchError::RateLimited,
        403 if exhausted => FetchError::RateLimited,
        s => FetchError::Http(s),
    }
}

fn classify(e: &reqwest::Error) -> FetchError {
    if e.is_timeout() {
        return FetchError::Timeout;
    }
    let mut chain = Vec::new();
    let mut source: Option<&dyn std::error::Error> = Some(e);
    while let Some(err) = source {
        chain.push(err.to_string());
        source = err.source();
    }
    let all = chain.join(": ").to_ascii_lowercase();
    let detail = chain.last().cloned().unwrap_or_default().chars().take(160).collect::<String>();
    if all.contains("timed out") {
        FetchError::Timeout
    } else if all.contains("dns error")
        || all.contains("failed to lookup address")
        || all.contains("name or service not known")
        || all.contains("temporary failure in name resolution")
        || all.contains("no address associated")
    {
        FetchError::Dns
    } else if all.contains("certificate") || all.contains("tls") || all.contains("handshake") {
        FetchError::Tls(detail)
    } else if e.is_connect() {
        FetchError::Connect(detail)
    } else {
        FetchError::Other(detail)
    }
}

/// A published release as the API describes it.
pub struct Release {
    pub tag: String,
    pub draft: bool,
    pub prerelease: bool,
    assets: Vec<Asset>,
}

struct Asset {
    name: String,
    id: u64,
    size: u64,
    sha256: Option<String>,
}

impl Release {
    pub fn from_json(v: &serde_json::Value) -> Option<Release> {
        let tag = v.get("tag_name")?.as_str()?.trim().to_string();
        if tag.is_empty() {
            return None;
        }
        let assets = v
            .get("assets")
            .and_then(|a| a.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|a| {
                        Some(Asset {
                            name: a.get("name")?.as_str()?.to_string(),
                            id: a.get("id")?.as_u64()?,
                            size: a.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                            sha256: a
                                .get("digest")
                                .and_then(|d| d.as_str())
                                .and_then(|d| d.strip_prefix("sha256:"))
                                .map(str::to_ascii_lowercase),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Release {
            tag,
            draft: v.get("draft").and_then(|d| d.as_bool()).unwrap_or(false),
            prerelease: v.get("prerelease").and_then(|d| d.as_bool()).unwrap_or(false),
            assets,
        })
    }

    /// Where to fetch `name` from, or `None` when the release doesn't include it.
    pub fn source(&self, repo: &str, name: &str) -> Option<Source> {
        let asset = self.assets.iter().find(|a| a.name == name)?;
        Some(Source {
            urls: vec![
                format!("https://github.com/{repo}/releases/download/{}/{name}", self.tag),
                format!("https://api.github.com/repos/{repo}/releases/assets/{}", asset.id),
            ],
            size: (asset.size > 0).then_some(asset.size),
            sha256: asset.sha256.clone(),
            listed: true,
        })
    }
}

/// One file and the URLs that serve it, tried in order.
pub struct Source {
    urls: Vec<String>,
    size: Option<u64>,
    sha256: Option<String>,
    /// The release listing names this file, so a 404 is a GitHub glitch.
    listed: bool,
}

impl Source {
    /// A file known only by URL, used when the release listing is unavailable.
    pub fn url(url: String) -> Source {
        Source { urls: vec![url], size: None, sha256: None, listed: false }
    }

    /// The release-download link for `name`; `tag: None` means latest.
    pub fn release_link(repo: &str, tag: Option<&str>, name: &str) -> Source {
        Source::url(match tag {
            Some(t) => format!("https://github.com/{repo}/releases/download/{t}/{name}"),
            None => format!("https://github.com/{repo}/releases/latest/download/{name}"),
        })
    }
}

/// Runs `attempt` against each URL, retrying transient failures in rounds.
/// `notify` hears a progress line before each retry.
async fn with_retries<T, F, Fut>(
    urls: &[String],
    listed: bool,
    delays: &[Duration],
    notify: &(dyn Fn(&str) + Sync),
    mut attempt: F,
) -> Result<T, FetchError>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<T, FetchError>>,
{
    let rounds = delays.len() + 1;
    let mut reported: Option<FetchError> = None;
    for round in 0..rounds {
        if round > 0 {
            if let Some(e) = &reported {
                let mut line = e.reason();
                if let Some(first) = line.get(..1) {
                    line = first.to_uppercase() + &line[1..];
                }
                notify(&format!("{line}. Trying again ({} of {rounds})…", round + 1));
            }
            tokio::time::sleep(delays[round - 1]).await;
        }
        for url in urls {
            match attempt(url.clone()).await {
                Ok(v) => return Ok(v),
                Err(FetchError::NotFound) if !listed => return Err(FetchError::NotFound),
                Err(FetchError::Local(d)) => return Err(FetchError::Local(d)),
                Err(e) => {
                    tracing::warn!("release download: {url}: {}", e.reason());
                    // A listed file that 404s is GitHub misbehaving, not a missing file.
                    let e = if e == FetchError::NotFound { FetchError::Http(404) } else { e };
                    // The API's rate limit says less about GitHub than the link's own error.
                    if !(e == FetchError::RateLimited && reported.is_some()) {
                        reported = Some(e);
                    }
                }
            }
        }
    }
    debug_assert!(reported.as_ref().is_some_and(FetchError::retryable));
    Err(reported.unwrap_or(FetchError::Other("no download source".into())))
}

fn is_api(url: &str) -> bool {
    url.starts_with("https://api.github.com/")
}

async fn get_json_once(url: String) -> Result<serde_json::Value, FetchError> {
    let resp = client()
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .timeout(SMALL_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| classify(&e))?;
    if !resp.status().is_success() {
        return Err(status_error(resp.status(), resp.headers()));
    }
    resp.json().await.map_err(|e| {
        if e.is_decode() { FetchError::Other("GitHub sent an unreadable reply".into()) } else { classify(&e) }
    })
}

/// GETs a GitHub API document, retrying transient failures.
pub async fn get_json(url: &str, notify: &(dyn Fn(&str) + Sync)) -> Result<serde_json::Value, FetchError> {
    with_retries(&[url.to_string()], false, &RETRY_DELAYS, notify, get_json_once).await
}

/// Looks up a release and its files; `tag: None` means the latest stable one.
pub async fn lookup_release(
    repo: &str,
    tag: Option<&str>,
    notify: &(dyn Fn(&str) + Sync),
) -> Result<Release, FetchError> {
    let url = match tag {
        Some(t) => format!("https://api.github.com/repos/{repo}/releases/tags/{t}"),
        None => format!("https://api.github.com/repos/{repo}/releases/latest"),
    };
    let json = get_json(&url, notify).await?;
    Release::from_json(&json).ok_or_else(|| FetchError::Other("GitHub's release listing was incomplete".into()))
}

async fn probe_once(url: String) -> Result<(), FetchError> {
    let mut req = client().head(&url).timeout(SMALL_REQUEST_TIMEOUT);
    if is_api(&url) {
        req = req.header(reqwest::header::ACCEPT, "application/octet-stream");
    }
    let resp = req.send().await.map_err(|e| classify(&e))?;
    if resp.status().is_success() { Ok(()) } else { Err(status_error(resp.status(), resp.headers())) }
}

/// Confirms that GitHub will serve `source`, without downloading it.
pub async fn probe(source: &Source, notify: &(dyn Fn(&str) + Sync)) -> Result<(), FetchError> {
    with_retries(&source.urls, source.listed, &RETRY_DELAYS, notify, probe_once).await
}

async fn download_once(
    url: &str,
    source: &Source,
    dest: &Path,
    progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
) -> Result<(), FetchError> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let mut req = client().get(url);
    if is_api(url) {
        req = req.header(reqwest::header::ACCEPT, "application/octet-stream");
    }
    let resp = req.send().await.map_err(|e| classify(&e))?;
    if !resp.status().is_success() {
        return Err(status_error(resp.status(), resp.headers()));
    }
    let total = source.size.or(resp.content_length().filter(|t| *t > 0));
    let local = |e: std::io::Error| FetchError::Local(format!("couldn't save the download to {} ({e})", dest.display()));
    // Truncates a partial file left by an earlier attempt.
    let mut file = tokio::fs::File::create(dest).await.map_err(local)?;
    let mut stream = resp.bytes_stream();
    let mut done: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| classify(&e))?;
        done += chunk.len() as u64;
        if done > MAX_ASSET_BYTES {
            return Err(FetchError::Integrity("GitHub sent a file larger than any release file".into()));
        }
        file.write_all(&chunk).await.map_err(local)?;
        progress(done, total);
    }
    file.flush().await.map_err(local)?;
    file.sync_all().await.map_err(local)?;
    drop(file);

    if done == 0 {
        return Err(FetchError::Integrity("GitHub sent an empty file".into()));
    }
    if let Some(expected) = source.size {
        if done != expected {
            return Err(FetchError::Integrity(format!(
                "the download was cut short ({done} of {expected} bytes)"
            )));
        }
    }
    if let Some(expected) = &source.sha256 {
        let path = dest.to_path_buf();
        let actual = tokio::task::spawn_blocking(move || sha256_file(&path))
            .await
            .map_err(|e| FetchError::Local(e.to_string()))?
            .map_err(local)?;
        if &actual != expected {
            return Err(FetchError::Integrity("the downloaded file didn't match GitHub's checksum".into()));
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hex::encode(hash.finish().as_ref()))
}

/// Downloads `source` to `dest`, verifying size and checksum when GitHub
/// lists them. `progress` hears (bytes so far, total if known).
pub async fn download(
    source: &Source,
    dest: &Path,
    notify: &(dyn Fn(&str) + Sync),
    progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
) -> Result<(), FetchError> {
    let result = download_with_delays(source, dest, &RETRY_DELAYS, notify, progress).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(dest).await;
    }
    result
}

async fn download_with_delays(
    source: &Source,
    dest: &Path,
    delays: &[Duration],
    notify: &(dyn Fn(&str) + Sync),
    progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
) -> Result<(), FetchError> {
    // FnMut closures can't lend `progress` to a returned future, so the
    // attempts, which run one at a time, share it through an async mutex.
    let progress = tokio::sync::Mutex::new(progress);
    with_retries(&source.urls, source.listed, delays, notify, |url| {
        let progress = &progress;
        async move {
            let mut guard = progress.lock().await;
            download_once(&url, source, dest, &mut **guard).await
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn statuses_map_to_actionable_errors() {
        let none = reqwest::header::HeaderMap::new();
        assert_eq!(status_error(reqwest::StatusCode::NOT_FOUND, &none), FetchError::NotFound);
        assert_eq!(status_error(reqwest::StatusCode::GATEWAY_TIMEOUT, &none), FetchError::Http(504));
        assert_eq!(status_error(reqwest::StatusCode::TOO_MANY_REQUESTS, &none), FetchError::RateLimited);
        assert_eq!(status_error(reqwest::StatusCode::FORBIDDEN, &none), FetchError::Http(403));
        let mut limited = reqwest::header::HeaderMap::new();
        limited.insert("x-ratelimit-remaining", "0".parse().unwrap());
        assert_eq!(status_error(reqwest::StatusCode::FORBIDDEN, &limited), FetchError::RateLimited);
    }

    #[test]
    fn a_github_outage_reads_as_one() {
        assert_eq!(
            FetchError::Http(504).sentence(),
            "GitHub's servers returned an error (HTTP 504). This is a problem on GitHub's side, not your Pi. Try again in a few minutes."
        );
        assert!(FetchError::Dns.sentence().starts_with("The Pi couldn't look up github.com."));
        assert_eq!(FetchError::NotFound.sentence(), "GitHub has no such file.");
    }

    fn release_json() -> serde_json::Value {
        serde_json::json!({
            "tag_name": "v3.28.8", "draft": false, "prerelease": true,
            "assets": [
                {"name": "sentryusb-linux-arm64-a76", "id": 42, "size": 21498304,
                 "digest": "sha256:ABCDEF"},
                {"name": "sentryusb-linux-armv7", "id": 43, "size": 0}
            ]
        })
    }

    #[test]
    fn listed_files_come_from_the_link_then_the_api() {
        let r = Release::from_json(&release_json()).unwrap();
        assert_eq!(r.tag, "v3.28.8");
        assert!(r.prerelease && !r.draft);
        let s = r.source("Sentry-Six/Sentry-USB-Rusty", "sentryusb-linux-arm64-a76").unwrap();
        assert_eq!(s.urls, [
            "https://github.com/Sentry-Six/Sentry-USB-Rusty/releases/download/v3.28.8/sentryusb-linux-arm64-a76",
            "https://api.github.com/repos/Sentry-Six/Sentry-USB-Rusty/releases/assets/42",
        ]);
        assert_eq!(s.size, Some(21498304));
        assert_eq!(s.sha256.as_deref(), Some("abcdef"));
        assert!(s.listed);
        assert_eq!(r.source("x/y", "sentryusb-linux-armv7").unwrap().size, None);
        assert!(r.source("x/y", "sentryusb-linux-arm64-a53").is_none());
    }

    #[test]
    fn unlisted_links_follow_latest_or_the_tag() {
        assert_eq!(Source::release_link("o/r", None, "f").urls, ["https://github.com/o/r/releases/latest/download/f"]);
        assert_eq!(Source::release_link("o/r", Some("v1"), "f").urls, ["https://github.com/o/r/releases/download/v1/f"]);
    }

    /// Serves one canned response per connection, in order.
    async fn serve(responses: Vec<&'static str>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                socket.write_all(response.as_bytes()).await.unwrap();
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{addr}/file")
    }

    const GATEWAY_TIMEOUT: &str = "HTTP/1.1 504 Gateway Timeout\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const NOT_FOUND: &str = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const OK_HELLO: &str = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
    const OK_SHORT: &str = "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nhel";

    #[tokio::test]
    async fn retries_through_gateway_timeouts_and_reports_each_retry() {
        let url = serve(vec![GATEWAY_TIMEOUT, GATEWAY_TIMEOUT, OK_HELLO]).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out");
        let source = Source { urls: vec![url], size: Some(5), sha256: Some(
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".into()), listed: true };
        let notes = std::sync::Mutex::new(Vec::new());
        let notify = |m: &str| notes.lock().unwrap().push(m.to_string());
        let mut seen = 0;
        download_with_delays(&source, &dest, &[Duration::ZERO; 2], &notify, &mut |done, _| seen = done)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        assert_eq!(seen, 5);
        assert_eq!(*notes.lock().unwrap(), [
            "GitHub's servers returned an error (HTTP 504). Trying again (2 of 3)…",
            "GitHub's servers returned an error (HTTP 504). Trying again (3 of 3)…",
        ]);
    }

    #[tokio::test]
    async fn gives_up_with_the_last_reason_and_cleans_up() {
        let url = serve(vec![OK_SHORT, OK_SHORT]).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out");
        let source = Source { urls: vec![url], size: Some(5), sha256: None, listed: true };
        let err = download_with_delays(&source, &dest, &[Duration::ZERO], &|_| {}, &mut |_, _| {})
            .await
            .unwrap_err();
        assert_eq!(err, FetchError::Integrity("the download was cut short (3 of 5 bytes)".into()));
    }

    #[tokio::test]
    async fn an_unlisted_404_is_final_but_a_listed_one_is_retried() {
        let url = serve(vec![NOT_FOUND]).await;
        let err = with_retries(&[url], false, &[Duration::ZERO; 2], &|_| {}, probe_once).await.unwrap_err();
        assert_eq!(err, FetchError::NotFound);

        let url = serve(vec![NOT_FOUND, OK_HELLO]).await;
        with_retries(&[url], true, &[Duration::ZERO; 2], &|_| {}, probe_once).await.unwrap();
    }

    #[tokio::test]
    async fn falls_back_to_the_second_source_within_a_round() {
        let broken = serve(vec![GATEWAY_TIMEOUT]).await;
        let working = serve(vec![OK_HELLO]).await;
        let notes = std::sync::Mutex::new(0);
        with_retries(&[broken, working], true, &[Duration::ZERO; 2], &|_| *notes.lock().unwrap() += 1, probe_once)
            .await
            .unwrap();
        assert_eq!(*notes.lock().unwrap(), 0, "no retry round was needed");
    }
}
