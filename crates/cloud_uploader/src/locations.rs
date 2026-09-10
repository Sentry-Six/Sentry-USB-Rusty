//! Time-aligned location evidence and guarded historical summary enrichment.
mod background;
pub(crate) use background::run_loop;
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};
use sentryusb_drives::{DriveStore, types::Route};
use sentryusb_cloud_crypto::{aad, aead, ids};
use serde_json::Value;

fn nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where D: serde::Deserializer<'de>, T: Deserialize<'de> {
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SummarySource {
    pub route_id: String,
    pub record_version: String,
    pub wrapped_route_key: String,
    #[serde(deserialize_with = "nullable")]
    pub summary_ciphertext: Option<String>,
    pub blob_len: usize,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SummaryProposal {
    pub route_id: String,
    pub record_version: String,
    pub wrapped_route_key: String,
    #[serde(deserialize_with = "nullable")]
    pub expected_summary_ciphertext: Option<String>,
    pub summary_ciphertext: String,
    pub operation_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PreparedSummary {
    pub source_fingerprint: String,
    pub proposal: SummaryProposal,
}

fn hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn checked_envelope(value: &str, max: usize, exact: Option<usize>) -> Result<Vec<u8>> {
    ensure!(value.len() <= max.div_ceil(3) * 4, "location envelope exceeds its limit");
    let bytes = B64.decode(value)?;
    ensure!((29..=max).contains(&bytes.len()) && bytes[0] == 1 && B64.encode(&bytes) == value
        && exact.is_none_or(|size| size == bytes.len()), "invalid location envelope");
    Ok(bytes)
}

fn matching_source(local: &Route, original: &Route) -> bool {
    local.file == original.file && local.points == original.points && !original.points.is_empty()
        && local.speeds == original.speeds && local.gear_states == original.gear_states
        && local.autopilot_states == original.autopilot_states
        && local.source.as_deref().unwrap_or("sei") == "sei"
        && original.source.as_deref().unwrap_or("sei") == "sei"
        && local.gear_runs.len() == original.gear_runs.len()
        && local.gear_runs.iter().zip(&original.gear_runs).all(|(a,b)| a.gear == b.gear && a.frames == b.frames)
        && (original.raw_frame_count == 0 || local.raw_frame_count == original.raw_frame_count)
}

fn label(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value.chars().any(|c| c <= '\u{001f}' || c == '\u{007f}') { return None; }
    // Keep the summary's existing 80 UTF-16-unit limit without splitting a
    // surrogate pair. The complete label remains in the original source.
    let mut units = 0;
    Some(value.chars().take_while(|c| { units += c.len_utf16(); units <= 80 }).collect())
}
fn missing(value: Option<&Value>) -> bool {
    value.is_none_or(|value| value.is_null() || value.as_str() == Some(""))
}
pub(crate) fn evidence_fingerprint(local: &Route, readings: Option<&LocationReadings>) -> Result<String> {
    let bytes = serde_json::to_vec(&(local, readings))?;
    Ok(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// Prepare a frozen repair from an authenticated original blob. This only
/// returns encrypted metadata; publication still needs a durable attempt and
/// current-pairing/source checks in the background worker.
pub(crate) fn prepare_summary(local: &Route, readings: Option<&LocationReadings>, blob: &[u8], source: &SummarySource,
    pi_key: &[u8;32], user_id: &str, pi_id: &str) -> Result<Option<PreparedSummary>> {
    ensure!(!user_id.is_empty() && !pi_id.is_empty() && hex(&source.route_id) && hex(&source.record_version), "invalid summary source identity");
    ensure!(blob.len() == source.blob_len && (29..=256*1024).contains(&blob.len()) && blob[0] == 1, "original route length changed");
    checked_envelope(&source.wrapped_route_key, 61, Some(61))?;
    let mut key = crate::encrypt::unwrap_content_key(pi_key, &source.wrapped_route_key, &aad::route_key(user_id, pi_id, &source.route_id))?;
    let result = (|| -> Result<Option<PreparedSummary>> {
        let plain = aead::open(&aead::Key::from_bytes(&key)?, &aad::route_blob(user_id, pi_id, &source.route_id), blob)?;
        let document: Value = serde_json::from_slice(&plain)?;
        let original: Route = serde_json::from_value(document.clone()).context("decode original route")?;
        ensure!(ids::route_id_from_path(&original.file) == source.route_id, "original route identity disagrees with its envelope");
        let mut summary: Value = match &source.summary_ciphertext {
            Some(value) => {
                checked_envelope(value, 4096, None)?;
                crate::encrypt::open_json_b64(&key, &aad::route_summary(user_id, pi_id, &source.route_id), value)?
            }
            None => crate::encrypt::route_summary_json(&original),
        };
        ensure!(summary.is_object() && summary["v"].as_u64().is_some_and(|version| (1..=4).contains(&version))
            && summary["file"].as_str() == Some(original.file.as_str()), "unsupported or mismatched original summary");
        let before = summary.clone();
        let matched = matching_source(local, &original);
        for (field, original_name, local_name) in [
            ("ls", original.location_name_start.as_deref(), local.location_name_start.as_deref()),
            ("le", original.location_name_end.as_deref(), local.location_name_end.as_deref()),
        ] {
            if !missing(summary.get(field)) { continue; }
            let original_label = label(original_name);
            let local_label = label(local_name);
            if original_label.is_none() && local_label.is_some() { ensure!(matched, "restored route differs from the original upload"); }
            if let Some(value) = original_label.or(local_label) { summary[field] = Value::String(value); }
        }
        if summary.get("lr").is_none() && has_internal_park(&original) {
            let gear: Vec<u32> = original.gear_runs.iter().flat_map(|run| [u32::from(run.gear), run.frames]).collect();
            if summary.get("gr") == Some(&serde_json::to_value(gear)?) {
                let candidate = match document.get("locationReadings") {
                    Some(value) => serde_json::from_value::<LocationReadings>(value.clone()).ok().filter(LocationReadings::valid),
                    None => {
                        if readings.is_some() { ensure!(matched, "restored route differs from the original upload"); }
                        readings.filter(|value| value.valid()).cloned()
                    }
                };
                if let Some(value) = candidate { summary["lr"] = serde_json::to_value(value)?; }
            }
        }
        crate::native_metrics::attach(&mut summary, &original);
        crate::summon_evidence::attach(&mut summary, &original, user_id, pi_id, &source.route_id, &source.wrapped_route_key);
        ensure!(serde_json::to_vec(&summary)?.len() + 29 <= 4096, "enriched summary exceeds its limit");
        if source.summary_ciphertext.is_some() && summary == before { return Ok(None); }
        let ciphertext = crate::encrypt::seal_json_b64(&key, &aad::route_summary(user_id, pi_id, &source.route_id), &summary)?;
        let mut nonce = [0u8;32];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce).map_err(|_|anyhow::anyhow!("summary operation randomness failed"))?;
        Ok(Some(PreparedSummary { source_fingerprint: evidence_fingerprint(local, readings)?, proposal: SummaryProposal {
            route_id: source.route_id.clone(), record_version: source.record_version.clone(), wrapped_route_key: source.wrapped_route_key.clone(),
            expected_summary_ciphertext: source.summary_ciphertext.clone(), summary_ciphertext: ciphertext,
            operation_id: nonce.iter().map(|b| format!("{b:02x}")).collect(),
        }}))
    })();
    key.fill(0);
    result
}

// This client never follows redirects, and adds Pi authorization only to API
// POSTs. Storage downloads use a separate request without credentials.
#[derive(Clone)]
pub(crate) struct SourceIdentity {
    pub route_id: String,
    pub record_version: String,
    pub wrapped_route_key: String,
    pub summary_ciphertext: Option<String>,
}

pub(crate) struct SummaryClient {
    http: reqwest::Client,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SummaryOutcome { Applied, Conflict, SourceChanged, NotFound, Cancelled }

pub(crate) trait SummaryTransport: Sync {
    fn states(&self, creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1, token: &[u8], ids: &[String])
        -> impl std::future::Future<Output=Result<std::collections::HashMap<String, Option<SourceIdentity>>>> + Send;
    fn original(&self, creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1, token: &[u8], source: &SourceIdentity)
        -> impl std::future::Future<Output=Result<(SummarySource, Vec<u8>)>> + Send;
    fn publish(&self, creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1, token: &[u8], proposal: &SummaryProposal, cancel: bool)
        -> impl std::future::Future<Output=Result<SummaryOutcome>> + Send;
}

impl SummaryTransport for SummaryClient {
    async fn states(&self, creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1, token: &[u8], ids: &[String])
        -> Result<std::collections::HashMap<String, Option<SourceIdentity>>> {
        ensure!(!ids.is_empty() && ids.len() <= 20 && ids.iter().all(|id| hex(id)), "invalid summary source request");
        let response = self.post(&creds.cloud_base_url, token, "/api/pi/sync/state", &serde_json::json!({
            "piId":creds.pi_id,"dekRotationGeneration":creds.dek_rotation_generation,
            "items":ids.iter().map(|id|serde_json::json!({"kind":"route","id":id})).collect::<Vec<_>>()
        })).await?;
        parse_states(response, ids)
    }
    async fn original(&self, creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1, token: &[u8], source: &SourceIdentity)
        -> Result<(SummarySource, Vec<u8>)> { self.original_blob(creds, token, source).await }
    async fn publish(&self, creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1, token: &[u8], proposal: &SummaryProposal, cancel: bool)
        -> Result<SummaryOutcome> { SummaryClient::publish(self, creds, token, proposal, cancel).await }
}

fn parse_states(response: Value, ids: &[String]) -> Result<std::collections::HashMap<String, Option<SourceIdentity>>> {
    let wanted: std::collections::HashSet<_> = ids.iter().collect();
    let items = response["items"].as_array().context("missing summary sources")?;
    ensure!(response["ok"] == true && response["writeProtocol"] == 3 && wanted.len() == ids.len()
        && items.len() == ids.len(), "incomplete summary sources");
    let mut result = std::collections::HashMap::new();
    for item in items {
        let id = item["id"].as_str().context("invalid summary source id")?.to_owned();
        ensure!(item["kind"] == "route" && wanted.contains(&id) && !result.contains_key(&id), "unexpected summary source identity");
        let source = match item["status"].as_str() {
            Some("not_found") => None,
            Some("ok") => {
                let version = item["recordVersion"].as_str().context("missing source version")?;
                let wrapped = item["wrappedKey"].as_str().context("missing source key")?;
                ensure!(hex(version), "invalid summary source version");
                checked_envelope(wrapped, 61, Some(61))?;
                let summary = match item.get("summaryCiphertext").context("missing summary ciphertext field")? {
                    Value::Null => None,
                    Value::String(value) => { checked_envelope(value, 4096, None)?; Some(value.clone()) }
                    _ => anyhow::bail!("invalid summary ciphertext field"),
                };
                Some(SourceIdentity {route_id:id.clone(),record_version:version.into(),wrapped_route_key:wrapped.into(),summary_ciphertext:summary})
            }
            _ => anyhow::bail!("unsupported summary source status"),
        };
        result.insert(id,source);
    }
    Ok(result)
}

impl SummaryClient {
    pub fn new() -> Result<Self> {
        Ok(Self { http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build().context("create location sync client")? })
    }

    async fn post(&self, base: &str, token: &[u8], path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{}", base.trim_end_matches('/'), path);
        let response = self.http.post(url).bearer_auth(B64.encode(token)).json(body).send().await
            .map_err(|_| anyhow::anyhow!("location sync request did not complete"))?;
        ensure!(response.status() == reqwest::StatusCode::OK, "location sync request was not confirmed (HTTP {})", response.status().as_u16());
        let bytes = bounded_response(response, 512 * 1024, None).await?;
        serde_json::from_slice(&bytes).context("invalid location sync response")
    }

    pub async fn original_blob(&self, creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1,
        token: &[u8], source: &SourceIdentity) -> Result<(SummarySource, Vec<u8>)> {
        ensure!(hex(&source.route_id) && hex(&source.record_version), "invalid original route identity");
        checked_envelope(&source.wrapped_route_key, 61, Some(61))?;
        if let Some(summary) = &source.summary_ciphertext { checked_envelope(summary, 4096, None)?; }
        let response = self.post(&creds.cloud_base_url, token, "/api/pi/sync/route-sources/v1", &serde_json::json!({
            "piId":creds.pi_id, "dekRotationGeneration":creds.dek_rotation_generation,
            "items":[{"routeId":source.route_id, "recordVersion":source.record_version,
                "wrappedRouteKey":source.wrapped_route_key, "expectedSummaryCiphertext":source.summary_ciphertext}]
        })).await?;
        let (source, url) = source_lease(response, creds, source)?;
        let blob = self.download(url, source.blob_len).await?;
        Ok((source, blob))
    }

    // The caller must obtain this URL from source_lease, which validates the
    // R2 origin. Kept separate to test the actual credential-free request.
    async fn download(&self, url: reqwest::Url, length: usize) -> Result<Vec<u8>> {
        ensure!((29..=256*1024).contains(&length), "invalid original route length");
        let response = self.http.get(url).send().await
            .map_err(|_| anyhow::anyhow!("original route download did not complete"))?;
        ensure!(response.status() == reqwest::StatusCode::OK, "original route download unavailable (HTTP {})", response.status().as_u16());
        bounded_response(response, 256 * 1024, Some(length)).await
    }

    /// Call only after the exact prepared operation (including cancellation
    /// mode) is durable. A lost/invalid response must leave that attempt pending.
    pub async fn publish(&self, creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1,
        token: &[u8], proposal: &SummaryProposal, cancel: bool) -> Result<SummaryOutcome> {
        validate_proposal(proposal)?;
        let path = if cancel { "/api/pi/sync/route-summaries/cancel/v1" } else { "/api/pi/sync/route-summaries/v1" };
        let response = self.post(&creds.cloud_base_url, token, path, &serde_json::json!({
            "piId":creds.pi_id,"dekRotationGeneration":creds.dek_rotation_generation,"items":[proposal]
        })).await?;
        acknowledgement(response, proposal)
    }
}

fn validate_source(source: &SummarySource) -> Result<()> {
    ensure!(hex(&source.route_id) && hex(&source.record_version) && (29..=256*1024).contains(&source.blob_len), "invalid original route source");
    checked_envelope(&source.wrapped_route_key, 61, Some(61))?;
    if let Some(summary) = &source.summary_ciphertext { checked_envelope(summary, 4096, None)?; }
    Ok(())
}
fn validate_proposal(proposal: &SummaryProposal) -> Result<()> {
    ensure!(hex(&proposal.route_id) && hex(&proposal.record_version) && hex(&proposal.operation_id), "invalid summary operation identity");
    checked_envelope(&proposal.wrapped_route_key, 61, Some(61))?;
    checked_envelope(&proposal.summary_ciphertext, 4096, None)?;
    if let Some(summary) = &proposal.expected_summary_ciphertext { checked_envelope(summary, 4096, None)?; }
    Ok(())
}

fn storage_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw).map_err(|_| anyhow::anyhow!("invalid original route lease URL"))?;
    let host = url.host_str().unwrap_or("");
    ensure!(raw.len() <= 8192 && url.scheme() == "https" && url.username().is_empty()
        && url.password().is_none() && url.fragment().is_none() && url.port_or_known_default() == Some(443)
        && host.strip_suffix(".r2.cloudflarestorage.com").is_some_and(|prefix| !prefix.is_empty()
            && prefix.split('.').all(|label| !label.is_empty() && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))),
        "original route lease is not an HTTPS R2 URL");
    let expiry: Vec<_> = url.query_pairs().filter(|(key,_)| key == "X-Amz-Expires").collect();
    ensure!(expiry.len() == 1 && expiry[0].1 == "60", "unexpected original route lease lifetime");
    Ok(url)
}

fn source_lease(response: Value, creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1,
    expected: &SourceIdentity) -> Result<(SummarySource, reqwest::Url)> {
    ensure!(response["ok"] == true && response["protocol"] == 1 && response["userId"] == creds.user_id
        && response["piId"] == creds.pi_id && response["dekRotationGeneration"] == creds.dek_rotation_generation,
        "original route lease authority changed");
    let items = response["items"].as_array().context("missing original route lease")?;
    ensure!(items.len() == 1, "incomplete original route lease");
    let mut item = items[0].as_object().context("invalid original route lease")?.clone();
    let raw = item.remove("routeBlobUrl").context("missing original route URL")?;
    let actual: SummarySource = serde_json::from_value(Value::Object(item)).context("invalid original route source")?;
    validate_source(&actual)?;
    ensure!(actual.route_id == expected.route_id && actual.record_version == expected.record_version
        && actual.wrapped_route_key == expected.wrapped_route_key && actual.summary_ciphertext == expected.summary_ciphertext
         , "original route lease source changed");
    let url = storage_url(raw.as_str().context("invalid original route URL")?)?;
    Ok((actual, url))
}

async fn bounded_response(mut response: reqwest::Response, limit: usize, exact: Option<usize>) -> Result<Vec<u8>> {
    ensure!(response.content_length().is_none_or(|len| len <= limit as u64
        && exact.is_none_or(|expected| len == expected as u64)), "unexpected location response length");
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| anyhow::anyhow!("location response was interrupted"))? {
        ensure!(chunk.len() <= limit - bytes.len(), "location response exceeds its limit");
        bytes.extend_from_slice(&chunk);
    }
    ensure!(exact.is_none_or(|len| bytes.len() == len), "original route response length changed");
    Ok(bytes)
}

fn acknowledgement(response: Value, proposal: &SummaryProposal) -> Result<SummaryOutcome> {
    ensure!(response["ok"] == true && response["protocol"] == 1, "invalid summary acknowledgement protocol");
    let results = response["results"].as_array().context("missing summary acknowledgement")?;
    ensure!(results.len() == 1 && results[0]["routeId"] == proposal.route_id
        && results[0]["operationId"] == proposal.operation_id, "summary acknowledgement identity changed");
    serde_json::from_value(results[0]["status"].clone()).map_err(|_| anyhow::anyhow!("unsupported summary acknowledgement"))
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingSummary {
    version: u8,
    binding: String,
    pub file: String,
    pub prepared: PreparedSummary,
    pub cancel: bool,
}

fn pairing_binding(creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1) -> Result<String> {
    Ok(ring::digest::digest(&ring::digest::SHA256, &serde_json::to_vec(creds)?)
        .as_ref().iter().map(|byte| format!("{byte:02x}")).collect())
}
fn pending_key(binding: &str) -> String { format!("cloud_location_pending_v1:{binding}") }

impl PendingSummary {
    fn validate(&self, binding: &str) -> Result<()> {
        ensure!(self.version == 1 && self.binding == binding && hex(binding)
            && !self.file.is_empty() && self.file.len() <= 4096
            && ids::route_id_from_path(&self.file) == self.prepared.proposal.route_id
            && hex(&self.prepared.source_fingerprint), "invalid pending location repair");
        validate_proposal(&self.prepared.proposal)
    }

    pub fn load(store: &DriveStore, creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1)
        -> Result<Option<(String, Self)>> {
        let binding = pairing_binding(creds)?;
        let raw = store.with_read_conn(|conn| sentryusb_drives::schema::meta_get(conn, &pending_key(&binding)))?;
        raw.map(|raw| {
            ensure!(raw.len() <= 20 * 1024, "pending location repair exceeds its limit");
            let pending: Self = serde_json::from_str(&raw).context("pending location repair is unreadable")?;
            pending.validate(&binding)?;
            Ok((raw, pending))
        }).transpose()
    }

    /// The caller checks current local evidence before reserving and before
    /// sending. Only encrypted summaries, identity hashes and the local filename
    /// are stored here, never a Pi key, token, source URL or telemetry samples.
    pub async fn reserve(state: &crate::state::CloudStateInner,
        creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1,
        file: String, prepared: PreparedSummary) -> Result<(String, Self)> {
        let pending = Self { version: 1, binding: pairing_binding(creds)?, file, prepared, cancel: false };
        pending.validate(&pending.binding)?;
        let raw = serde_json::to_string(&pending)?;
        ensure!(raw.len() <= 20 * 1024, "pending location repair exceeds its limit");
        let _guard = state.current_credentials(creds).await?;
        pending.replace(&state.store, None, Some(&raw))?;
        Ok((raw, pending))
    }

    /// Cancellation mode is irreversible and must reach durable storage before
    /// its request leaves the Pi. An unknown result keeps this exact operation.
    pub async fn cancel(state: &crate::state::CloudStateInner,
        creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1, raw: &str,
        pending: &Self) -> Result<(String, Self)> {
        pending.validate(&pairing_binding(creds)?)?;
        pending.matches_raw(raw)?;
        let mut updated = pending.clone(); updated.cancel = true;
        let next = serde_json::to_string(&updated)?;
        let _guard = state.current_credentials(creds).await?;
        pending.replace(&state.store, Some(raw), Some(&next))?;
        Ok((next, updated))
    }

    fn matches_raw(&self, raw: &str) -> Result<()> {
        ensure!(serde_json::from_str::<Value>(raw)? == serde_json::to_value(self)?, "pending location repair disagrees with stored request");
        Ok(())
    }

    fn replace(&self, store: &DriveStore, expected: Option<&str>, next: Option<&str>) -> Result<()> {
        use sentryusb_drives::schema;
        let key = pending_key(&self.binding);
        store.with_durable_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            ensure!(schema::meta_get(&tx, &key)?.as_deref() == expected, "pending location repair changed");
            if let Some(next) = next { schema::meta_set(&tx, &key, next)?; }
            else { schema::meta_del(&tx, &key)?; }
            tx.commit()?; Ok(())
        })
    }

    /// The caller supplies freshly read evidence. Absence/deletion requests
    /// cancellation too; a changed local snapshot never authorizes a new replay.
    pub async fn advance(state: &crate::state::CloudStateInner,
        creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1, token: &[u8],
        client: &impl SummaryTransport, raw: String, pending: Self, fingerprint: Option<&str>) -> Result<(SummaryOutcome, bool)> {
        pending.validate(&pairing_binding(creds)?)?;
        pending.matches_raw(&raw)?;
        let (raw, pending) = if !pending.cancel && fingerprint != Some(pending.prepared.source_fingerprint.as_str()) {
            Self::cancel(state, creds, &raw, &pending).await?
        } else { (raw, pending) };
        {
            let _guard = state.current_credentials(creds).await?;
            // Also verify the exact persisted attempt before every send.
            let stored = state.store.with_read_conn(|conn| sentryusb_drives::schema::meta_get(conn, &pending_key(&pending.binding)))?;
            ensure!(stored.as_deref() == Some(&raw), "pending location repair changed before send");
        }
        let outcome = client.publish(creds, token, &pending.prepared.proposal, pending.cancel).await?;
        let _guard = state.current_credentials(creds).await?;
        pending.replace(&state.store, Some(&raw), None)?;
        Ok((outcome, pending.cancel))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct LocationReadings {
    pub v: u8,
    pub readings: Vec<(i64, f64, f64, String)>,
}

impl LocationReadings {
    pub fn valid(&self) -> bool {
        if self.v != 1 || self.readings.is_empty() || self.readings.len() > 61 { return false; }
        let mut previous = -1;
        for (offset, lat, lng, name) in &self.readings {
            if *offset < 0 || *offset > 60000 || *offset <= previous
                || !lat.is_finite() || !lng.is_finite() || lat.abs() > 90.0 || lng.abs() > 180.0
                || (*lat == 0.0 && *lng == 0.0) || name.is_empty() || name.trim() != name
                || name.len() > 240 || name.chars().any(|c| c <= '\u{001f}' || c == '\u{007f}') {
                return false;
            }
            previous = *offset;
        }
        serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= 2048)
    }
}

pub(crate) fn for_route(store: &DriveStore, route: &Route) -> Result<Option<LocationReadings>> {
    store.with_read_conn(|conn| for_route_on_conn(conn, route))
}

fn for_route_on_conn(conn: &rusqlite::Connection, route: &Route) -> Result<Option<LocationReadings>> {
    if route.source.as_deref().is_some_and(|source| source != "sei") { return Ok(None); }
    if !has_internal_park(route) { return Ok(None); }
    // An ambiguous DST filename cannot establish which telemetry hour belongs
    // to the clip. Keep the original route instead of assigning the other hour.
    let Some(clock) = sentryusb_drives::grouper::parse_clip_timestamp(&route.file) else { return Ok(None); };
    let Some(start) = Local.from_local_datetime(&clock).single() else { return Ok(None); };
    for_window_on_conn(conn, start.timestamp())
}

// Whole clips already carry endpoint labels. Extra samples are useful only
// when Park separates two moving portions of the same uploaded clip.
fn has_internal_park(route: &Route) -> bool {
    let mut moved = false;
    let mut parked_after_moving = false;
    for run in &route.gear_runs {
        if run.frames == 0 { return false; }
        if run.gear == 0 { parked_after_moving |= moved; }
        else {
            if parked_after_moving { return true; }
            moved = true;
        }
    }
    false
}

#[cfg(test)]
fn for_window(store: &DriveStore, start: i64) -> Result<Option<LocationReadings>> {
    store.with_read_conn(|conn| for_window_on_conn(conn, start))
}
fn for_window_on_conn(conn: &rusqlite::Connection, start: i64) -> Result<Option<LocationReadings>> {
    let Some(end) = start.checked_add(60) else { return Ok(None); };
        let mut stmt = conn.prepare_cached("SELECT ts,latitude,longitude,location_name FROM telemetry_samples
            WHERE ts BETWEEN ?1 AND ?2 AND location_name IS NOT NULL ORDER BY ts LIMIT 62")?;
        let rows = stmt.query_map(rusqlite::params![start,end], |row| Ok((
            row.get::<_,i64>(0)?, row.get::<_,Option<f64>>(1)?, row.get::<_,Option<f64>>(2)?, row.get::<_,String>(3)?
        )))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut readings = Vec::new();
        for (timestamp,lat,lng,name) in rows {
            let name = name.trim();
            if name.is_empty() { continue; }
            let (Some(lat),Some(lng)) = (lat,lng) else { return Ok(None); };
            readings.push(((timestamp-start)*1000,lat,lng,name.to_owned()));
        }
        let result = LocationReadings { v: 1, readings };
        Ok(result.valid().then_some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn credentials(base: String) -> sentryusb_cloud_crypto::credentials::CloudCredentialsV1 {
        use sentryusb_cloud_crypto::credentials::{CloudCredentialsV1,LongTermX25519OnDisk};
        CloudCredentialsV1 { version:1,user_id:"owner".into(),pi_id:"pi".into(),pi_auth_token:"synthetic".into(),
            wrapped_pi_key_local:"synthetic".into(),long_term_x25519:LongTermX25519OnDisk {
                public_key:"synthetic".into(),wrapped_private_key:"synthetic".into() },
            cloud_base_url:base,paired_at:chrono::Utc::now(),dek_rotation_generation:3 }
    }
    fn identity(source: &SummarySource) -> SourceIdentity {
        SourceIdentity {route_id:source.route_id.clone(),record_version:source.record_version.clone(),
            wrapped_route_key:source.wrapped_route_key.clone(),summary_ciphertext:source.summary_ciphertext.clone()}
    }
    // One bounded loopback request, with captured headers/body for protocol tests.
    async fn server(reply: Vec<u8>) -> (String, tokio::task::JoinHandle<String>) {
        let (url, task) = server_sequence(vec![reply]).await;
        (url, tokio::spawn(async move { task.await.unwrap().remove(0) }))
    }
    async fn server_sequence(replies: Vec<Vec<u8>>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt,AsyncWriteExt};
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base=format!("http://{}",listener.local_addr().unwrap());
        let task=tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(5),async move {
                let mut requests = Vec::new();
                for reply in replies {
                let (mut stream,_)=listener.accept().await.unwrap();let mut request=Vec::new();
                loop {
                    let mut chunk=[0;4096];let n=stream.read(&mut chunk).await.unwrap();assert!(n>0);request.extend_from_slice(&chunk[..n]);
                    assert!(request.len()<65536);
                    if let Some(end)=request.windows(4).position(|part|part==b"\r\n\r\n") {
                        let headers=String::from_utf8_lossy(&request[..end]);
                        let length=headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length: ").and_then(|v|v.parse::<usize>().ok())).unwrap_or(0);
                        if request.len()>=end+4+length {break;}
                    }
                }
                let _=stream.write_all(&reply).await;
                requests.push(String::from_utf8(request).unwrap());
                }
                requests
            }).await.unwrap()
        });
        (base,task)
    }
    fn reply(status: &str, body: &[u8]) -> Vec<u8> {
        [format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).into_bytes(),body.to_vec()].concat()
    }
    #[test]
    fn leases_bind_every_source_field_and_only_allow_short_https_r2_downloads() {
        let (source,_,_,_)=encrypted_source(&original_route());let expected=identity(&source);
        let creds=credentials("https://example.invalid".into());
        let mut item=serde_json::to_value(&source).unwrap();
        item["routeBlobUrl"]=Value::from("https://account.r2.cloudflarestorage.com/bucket/object?X-Amz-Expires=60");
        let good=serde_json::json!({"ok":true,"protocol":1,"userId":"owner","piId":"pi","dekRotationGeneration":3,"items":[item]});
        assert!(source_lease(good.clone(),&creds,&expected).is_ok());
        for (field,value) in [("routeId",Value::from("b".repeat(64))),("recordVersion",Value::from("b".repeat(64))),
            ("wrappedRouteKey",Value::from(B64.encode([1;61]))),("summaryCiphertext",Value::Null),("blobLen",Value::from(300000))] {
            let mut bad=good.clone();bad["items"][0][field]=value;assert!(source_lease(bad,&creds,&expected).is_err(),"{field}");
        }
        for field in ["userId","piId","dekRotationGeneration","protocol","ok"] {
            let mut bad=good.clone();bad[field]=Value::Null;assert!(source_lease(bad,&creds,&expected).is_err());
        }
        for url in ["http://account.r2.cloudflarestorage.com/o?X-Amz-Expires=60",
            "https://account.r2.cloudflarestorage.com.evil.invalid/o?X-Amz-Expires=60",
            "https://127.0.0.1/o?X-Amz-Expires=60","https://r2.cloudflarestorage.com/o?X-Amz-Expires=60",
            "https://user:pass@account.r2.cloudflarestorage.com/o?X-Amz-Expires=60",
            "https://account.r2.cloudflarestorage.com:444/o?X-Amz-Expires=60",
            "https://account.r2.cloudflarestorage.com/o?X-Amz-Expires=600",
            "https://account.r2.cloudflarestorage.com/o?X-Amz-Expires=60&X-Amz-Expires=60",
            "https://account.r2.cloudflarestorage.com/o?X-Amz-Expires=60#fragment"] {
            assert!(storage_url(url).is_err(),"{url}");
        }
    }
    #[tokio::test]
    async fn original_download_uses_no_bearer_and_requires_complete_bounded_body() {
        let client=SummaryClient::new().unwrap();
        let bytes=vec![1u8;61];let (url,task)=server(reply("200 OK",&bytes)).await;
        assert_eq!(client.download(reqwest::Url::parse(&url).unwrap(),61).await.unwrap(),bytes);
        let request=task.await.unwrap().to_ascii_lowercase();assert!(request.starts_with("get / "));
        assert!(!request.contains("authorization:") && !request.contains("cookie:"));
        for (status,body,length) in [("200 OK",vec![1;60],61),("206 Partial Content",vec![1;61],61),
            ("404 Not Found",vec![1;61],61)] {
            let (url,task)=server(reply(status,&body)).await;
            assert!(client.download(reqwest::Url::parse(&url).unwrap(),length).await.is_err());task.await.unwrap();
        }
        let huge=vec![1;256*1024+1];
        let mut response=b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        response.extend_from_slice(format!("{:x}\r\n",huge.len()).as_bytes());response.extend_from_slice(&huge);response.extend_from_slice(b"\r\n0\r\n\r\n");
        let (url,task)=server(response).await;
        assert!(client.download(reqwest::Url::parse(&url).unwrap(),61).await.is_err());task.await.unwrap();
    }
    #[tokio::test]
    async fn publication_and_cancellation_use_identical_frozen_operations() {
        let original=original_route();let (source,blob,_,_)=encrypted_source(&original);
        let mut local=original;local.location_name_start=Some("Library".into());
        let proposal=prepare_summary(&local,None,&blob,&source,&[9;32],"owner","pi").unwrap().unwrap().proposal;
        let client=SummaryClient::new().unwrap();let mut bodies=Vec::new();
        for cancel in [false,true] {
            let ack=serde_json::json!({"ok":true,"protocol":1,"results":[{"routeId":proposal.route_id,"operationId":proposal.operation_id,"status":"applied"}]});
            let (base,task)=server(reply("200 OK",&serde_json::to_vec(&ack).unwrap())).await;
            assert_eq!(client.publish(&credentials(base),&[3;32],&proposal,cancel).await.unwrap(),SummaryOutcome::Applied);
            let request=task.await.unwrap();
            let path=if cancel {"route-summaries/cancel/v1"} else {"route-summaries/v1"};
            assert!(request.starts_with(&format!("POST /api/pi/sync/{path} ")));
            assert!(request.to_ascii_lowercase().contains(&format!("authorization: bearer {}",B64.encode([3;32])).to_ascii_lowercase()));
            let body:Value=serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();bodies.push(body);
        }
        assert_eq!(bodies[0],bodies[1]);assert_eq!(bodies[0]["items"][0],serde_json::to_value(&proposal).unwrap());
        let good=serde_json::json!({"ok":true,"protocol":1,"results":[{"routeId":proposal.route_id,"operationId":proposal.operation_id,"status":"cancelled"}]});
        assert_eq!(acknowledgement(good.clone(),&proposal).unwrap(),SummaryOutcome::Cancelled);
        for (field,value) in [("routeId","wrong"),("operationId","wrong"),("status","pending")] {
            let mut bad=good.clone();bad["results"][0][field]=Value::from(value);assert!(acknowledgement(bad,&proposal).is_err());
        }
        let mut bad=good.clone();bad["results"].as_array_mut().unwrap().push(good["results"][0].clone());assert!(acknowledgement(bad,&proposal).is_err());
    }
    #[tokio::test]
    async fn redirect_and_transport_failures_do_not_expose_or_forward_credentials() {
        let target=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let response=format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{}/private?token=synthetic-secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",target.local_addr().unwrap());
        let (base,task)=server(response.into_bytes()).await;
        let client=SummaryClient::new().unwrap();
        let error=client.post(&base,&[3;32],"/api/pi/sync/route-sources/v1",&serde_json::json!({})).await.unwrap_err();
        assert!(!format!("{error:#}").contains("synthetic-secret"));task.await.unwrap();
        assert!(tokio::time::timeout(std::time::Duration::from_millis(25),target.accept()).await.is_err());
        drop(target);
        let socket=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let url=format!("http://{}/?secret=do-not-log",socket.local_addr().unwrap());drop(socket);
        let error=client.download(reqwest::Url::parse(&url).unwrap(),61).await.unwrap_err();
        assert!(!format!("{error:#}").contains("do-not-log"));
    }
    async fn journal_state(store: DriveStore, creds: &sentryusb_cloud_crypto::credentials::CloudCredentialsV1) -> crate::state::CloudStateInner {
        let state=crate::state::CloudStateInner::new(std::sync::Arc::new(store),sentryusb_ws::Hub::new(),
            std::sync::Arc::new(tokio::sync::Notify::new()),creds.cloud_base_url.clone(),String::new(),None);
        *state.creds.lock().await=Some(creds.clone());state
    }
    fn prepared() -> PreparedSummary {
        let original=original_route();let (source,blob,_,_)=encrypted_source(&original);
        let mut local=original;local.location_name_start=Some("Library".into());
        prepare_summary(&local,None,&blob,&source,&[9;32],"owner","pi").unwrap().unwrap()
    }
    fn ack_reply(prepared: &PreparedSummary, status: &str) -> Vec<u8> {
        reply("200 OK",&serde_json::to_vec(&serde_json::json!({"ok":true,"protocol":1,"results":[{
            "routeId":prepared.proposal.route_id,"operationId":prepared.proposal.operation_id,"status":status}]})).unwrap())
    }
    #[tokio::test]
    async fn uncertain_write_and_cancellation_survive_reopen_without_rebuilding_the_operation() {
        let prepared=prepared();let fingerprint=prepared.source_fingerprint.clone();
        let (base,requests)=server_sequence(vec![vec![],reply("503 Service Unavailable",b"{}"),ack_reply(&prepared,"applied")]).await;
        let creds=credentials(base);let temp=tempfile::tempdir().unwrap();let path=temp.path().join("journal.sqlite");
        let state=journal_state(crate::open_test_store(path.to_str().unwrap()).unwrap(),&creds).await;
        let (raw,pending)=PendingSummary::reserve(&state,&creds,original_route().file,prepared.clone()).await.unwrap();
        let client=SummaryClient::new().unwrap();
        assert!(PendingSummary::advance(&state,&creds,&[3;32],&client,raw.clone(),pending,Some(&fingerprint)).await.is_err());
        assert_eq!(PendingSummary::load(&state.store,&creds).unwrap().unwrap().0,raw);
        drop(state);
        let state=journal_state(crate::open_test_store(path.to_str().unwrap()).unwrap(),&creds).await;
        let (raw,pending)=PendingSummary::load(&state.store,&creds).unwrap().unwrap();
        assert!(PendingSummary::advance(&state,&creds,&[3;32],&client,raw,pending,None).await.is_err());
        let (cancelled_raw,pending)=PendingSummary::load(&state.store,&creds).unwrap().unwrap();assert!(pending.cancel);
        assert!(!cancelled_raw.contains("Library") && !cancelled_raw.contains("synthetic") && !cancelled_raw.contains("routeBlobUrl"));
        drop(state);
        let state=journal_state(crate::open_test_store(path.to_str().unwrap()).unwrap(),&creds).await;
        let (raw,pending)=PendingSummary::load(&state.store,&creds).unwrap().unwrap();assert_eq!(raw,cancelled_raw);
        assert_eq!(PendingSummary::advance(&state,&creds,&[3;32],&client,raw,pending,Some(&fingerprint)).await.unwrap(),(SummaryOutcome::Applied,true));
        assert!(PendingSummary::load(&state.store,&creds).unwrap().is_none());
        let requests=requests.await.unwrap();assert_eq!(requests.len(),3);
        assert!(requests[0].starts_with("POST /api/pi/sync/route-summaries/v1 "));
        assert!(requests[1..].iter().all(|r|r.starts_with("POST /api/pi/sync/route-summaries/cancel/v1 ")));
        let bodies:Vec<Value>=requests.iter().map(|r|serde_json::from_str(r.split("\r\n\r\n").nth(1).unwrap()).unwrap()).collect();
        assert_eq!(bodies[0],bodies[1]);assert_eq!(bodies[1],bodies[2]);
        assert_eq!(bodies[0]["items"][0],serde_json::to_value(&prepared.proposal).unwrap());
    }
    #[tokio::test]
    async fn journal_rejects_overwrite_changed_pairing_corruption_and_cancellation_reversal() {
        let creds=credentials("http://127.0.0.1:1".into());let state=journal_state(DriveStore::open_memory().unwrap(),&creds).await;
        let (raw,pending)=PendingSummary::reserve(&state,&creds,original_route().file,prepared()).await.unwrap();
        assert!(PendingSummary::reserve(&state,&creds,original_route().file,prepared()).await.is_err());
        let mut other=creds.clone();other.dek_rotation_generation+=1;
        assert!(PendingSummary::load(&state.store,&other).unwrap().is_none());
        assert!(PendingSummary::cancel(&state,&other,&raw,&pending).await.is_err());
        let mut altered=pending.clone();altered.prepared.proposal.operation_id="b".repeat(64);
        assert!(PendingSummary::cancel(&state,&creds,&raw,&altered).await.is_err());
        let (cancel_raw,cancelled)=PendingSummary::cancel(&state,&creds,&raw,&pending).await.unwrap();
        assert!(PendingSummary::cancel(&state,&creds,&raw,&pending).await.is_err());
        let mut reversal=cancelled.clone();reversal.cancel=false;
        assert!(PendingSummary::advance(&state,&creds,&[3;32],&SummaryClient::new().unwrap(),cancel_raw.clone(),reversal,None).await.is_err());
        *state.creds.lock().await=Some(other);
        assert!(PendingSummary::advance(&state,&creds,&[3;32],&SummaryClient::new().unwrap(),cancel_raw.clone(),cancelled,None).await.is_err());
        assert_eq!(PendingSummary::load(&state.store,&creds).unwrap().unwrap().0,cancel_raw);
        state.store.with_locked_conn(|conn| sentryusb_drives::schema::meta_set(conn,&pending_key(&pairing_binding(&creds).unwrap()),"{broken")).unwrap();
        assert!(PendingSummary::load(&state.store,&creds).is_err());
    }
    #[tokio::test]
    async fn failed_retirement_keeps_the_exact_attempt_for_receipt_replay() {
        let prepared=prepared();let fingerprint=prepared.source_fingerprint.clone();
        let (base,requests)=server_sequence(vec![ack_reply(&prepared,"applied"),ack_reply(&prepared,"applied")]).await;
        let creds=credentials(base);let state=journal_state(DriveStore::open_memory().unwrap(),&creds).await;
        let (raw,pending)=PendingSummary::reserve(&state,&creds,original_route().file,prepared).await.unwrap();
        state.store.with_locked_conn(|conn| conn.execute_batch("CREATE TRIGGER fail_location_retire BEFORE DELETE ON meta BEGIN SELECT RAISE(ABORT, 'synthetic retirement failure'); END;")).unwrap();
        let client=SummaryClient::new().unwrap();
        assert!(PendingSummary::advance(&state,&creds,&[3;32],&client,raw.clone(),pending,Some(&fingerprint)).await.is_err());
        assert_eq!(PendingSummary::load(&state.store,&creds).unwrap().unwrap().0,raw);
        state.store.with_locked_conn(|conn| conn.execute_batch("DROP TRIGGER fail_location_retire")).unwrap();
        let (raw,pending)=PendingSummary::load(&state.store,&creds).unwrap().unwrap();
        assert_eq!(PendingSummary::advance(&state,&creds,&[3;32],&client,raw,pending,Some(&fingerprint)).await.unwrap(),(SummaryOutcome::Applied,false));
        assert!(PendingSummary::load(&state.store,&creds).unwrap().is_none());
        let requests=requests.await.unwrap();assert_eq!(requests[0],requests[1]);
    }
    #[tokio::test]
    async fn original_source_request_is_bound_and_rejects_an_untrusted_download_target() {
        let (source,_,_,_)=encrypted_source(&original_route());
        let target=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut item=serde_json::to_value(&source).unwrap();
        item["routeBlobUrl"]=Value::from(format!("http://{}/private?X-Amz-Expires=60",target.local_addr().unwrap()));
        let response=serde_json::json!({"ok":true,"protocol":1,"userId":"owner","piId":"pi","dekRotationGeneration":3,"items":[item]});
        let (base,task)=server(reply("200 OK",&serde_json::to_vec(&response).unwrap())).await;
        assert!(SummaryClient::new().unwrap().original_blob(&credentials(base),&[3;32],&identity(&source)).await.is_err());
        let request=task.await.unwrap();assert!(request.starts_with("POST /api/pi/sync/route-sources/v1 "));
        let body:Value=serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body,serde_json::json!({"piId":"pi","dekRotationGeneration":3,"items":[{
            "routeId":source.route_id,"recordVersion":source.record_version,"wrappedRouteKey":source.wrapped_route_key,
            "expectedSummaryCiphertext":source.summary_ciphertext}]}));
        assert!(tokio::time::timeout(std::time::Duration::from_millis(25),target.accept()).await.is_err());
    }
    #[tokio::test]
    async fn failed_durable_cancellation_cannot_send_any_request() {
        let target=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let creds=credentials(format!("http://{}",target.local_addr().unwrap()));
        let state=journal_state(DriveStore::open_memory().unwrap(),&creds).await;
        let (raw,pending)=PendingSummary::reserve(&state,&creds,original_route().file,prepared()).await.unwrap();
        state.store.with_locked_conn(|conn| conn.execute_batch("CREATE TRIGGER fail_location_cancel BEFORE INSERT ON meta BEGIN SELECT RAISE(ABORT, 'synthetic cancellation failure'); END;")).unwrap();
        assert!(PendingSummary::advance(&state,&creds,&[3;32],&SummaryClient::new().unwrap(),raw.clone(),pending,None).await.is_err());
        let (saved,pending)=PendingSummary::load(&state.store,&creds).unwrap().unwrap();assert_eq!(saved,raw);assert!(!pending.cancel);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(25),target.accept()).await.is_err());
    }
    #[tokio::test]
    async fn source_state_reader_requires_complete_identity_and_explicit_nullable_summary() {
        let (source,_,_,_)=encrypted_source(&original_route());let ids=vec![source.route_id.clone()];
        let response=serde_json::json!({"ok":true,"writeProtocol":3,"items":[{"kind":"route","id":source.route_id,"status":"ok",
            "recordVersion":source.record_version,"wrappedKey":source.wrapped_route_key,"summaryCiphertext":source.summary_ciphertext}]});
        let (base,task)=server(reply("200 OK",&serde_json::to_vec(&response).unwrap())).await;
        let values=SummaryClient::new().unwrap().states(&credentials(base),&[3;32],&ids).await.unwrap();
        assert_eq!(values[&ids[0]].as_ref().unwrap().summary_ciphertext,source.summary_ciphertext);
        let request=task.await.unwrap();assert!(request.starts_with("POST /api/pi/sync/state "));
        let body:Value=serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["items"],serde_json::json!([{"kind":"route","id":ids[0]}]));
        let mut missing=response.clone();missing["items"][0].as_object_mut().unwrap().remove("summaryCiphertext");assert!(parse_states(missing,&ids).is_err());
        for (field,value) in [("kind",Value::from("charge")),("id",Value::from("b".repeat(64))),("status",Value::from("deleted")),
            ("recordVersion",Value::Null),("wrappedKey",Value::Null)] {
            let mut bad=response.clone();bad["items"][0][field]=value;assert!(parse_states(bad,&ids).is_err());
        }
        let mut empty=response.clone();empty["items"]=serde_json::json!([]);assert!(parse_states(empty,&ids).is_err());
        let mut duplicate=response.clone();duplicate["items"].as_array_mut().unwrap().push(response["items"][0].clone());assert!(parse_states(duplicate,&ids).is_err());
        let mut null=response.clone();null["items"][0]["summaryCiphertext"]=Value::Null;assert!(parse_states(null,&ids).unwrap()[&ids[0]].as_ref().unwrap().summary_ciphertext.is_none());
        let absent=serde_json::json!({"ok":true,"writeProtocol":3,"items":[{"kind":"route","id":ids[0],"status":"not_found"}]});
        assert!(parse_states(absent,&ids).unwrap()[&ids[0]].is_none());
    }
    fn original_route() -> Route {
        use sentryusb_drives::types::GearRun;
        Route { file: "2026-09-07_12-00-00-front.mp4".into(), date: "2026-09-07".into(),
            points: vec![[40.0,-73.0],[40.0001,-73.0001]], speeds: vec![1.0,2.0],
            raw_frame_count: 60, gear_states: (0..60).map(|i| if (10..30).contains(&i) {0} else {4}).collect(),
            gear_runs: vec![GearRun {gear:4,frames:10},GearRun {gear:0,frames:20},GearRun {gear:4,frames:30}],
            ..Default::default() }
    }
    fn encrypted_source(original: &Route) -> (SummarySource, Vec<u8>, [u8;32], Value) {
        let encrypted = crate::encrypt::encrypt_route(original,&[9;32],"owner","pi",None).unwrap();
        let key = crate::encrypt::unwrap_content_key(&[9;32],&encrypted.wrapped_route_key_b64,&aad::route_key("owner","pi",&encrypted.route_id)).unwrap();
        let mut summary = crate::encrypt::route_summary_json(original);
        crate::native_metrics::attach(&mut summary,original);
        summary.as_object_mut().unwrap().remove("ls"); summary.as_object_mut().unwrap().remove("le");
        summary["futureExtension"] = serde_json::json!({"keep":[1,"unchanged"]});
        let blob = B64.decode(&encrypted.route_blob_b64).unwrap();
        let source = SummarySource { route_id: encrypted.route_id, record_version: "a".repeat(64),
            wrapped_route_key: encrypted.wrapped_route_key_b64, blob_len: blob.len(),
            summary_ciphertext: None };
        let cipher = crate::encrypt::seal_json_b64(&key,&aad::route_summary("owner","pi",&source.route_id),&summary).unwrap();
        (SummarySource {summary_ciphertext:Some(cipher),..source},blob,key,summary)
    }
    #[test]
    fn preparation_checks_original_data_and_preserves_unknown_summary_fields() {
        let original = original_route(); let mut local = original.clone();
        local.location_name_start=Some("Library".into());local.location_name_end=Some("Home".into());
        let readings=LocationReadings {v:1,readings:vec![(20000,40.0,-73.0,"Stop".into())]};
        let (source,blob,key,before)=encrypted_source(&original);let original_bytes=blob.clone();
        let prepared=prepare_summary(&local,Some(&readings),&blob,&source,&[9;32],"owner","pi").unwrap().unwrap();
        let mut after:Value=crate::encrypt::open_json_b64(&key,&aad::route_summary("owner","pi",&source.route_id),&prepared.proposal.summary_ciphertext).unwrap();
        assert_eq!(after["ls"],"Library");assert_eq!(after["le"],"Home");assert_eq!(after["lr"],serde_json::to_value(&readings).unwrap());
        for field in ["ls","le","lr"] {after.as_object_mut().unwrap().remove(field);}
        assert_eq!(after,before);assert_eq!(blob,original_bytes);
        assert_eq!(prepared.proposal.wrapped_route_key,source.wrapped_route_key);
        assert_eq!(prepared.proposal.expected_summary_ciphertext,source.summary_ciphertext);
        assert_eq!(prepared.proposal.record_version,source.record_version);
        assert!(hex(&prepared.proposal.operation_id));assert!(hex(&prepared.source_fingerprint));
        let stored=serde_json::to_value(&prepared).unwrap();
        assert_eq!(stored.as_object().unwrap().len(),2);
        assert!(stored.get("readings").is_none());assert!(stored.get("key").is_none());
        assert!(serde_json::from_value::<PreparedSummary>(stored).is_ok());
    }
    #[test]
    fn a_matching_filename_cannot_hide_changed_route_data_or_wrong_crypto_context() {
        let original=original_route();let (source,blob,_,_)=encrypted_source(&original);
        let mut local=original.clone();local.location_name_start=Some("Library".into());local.points[1][0]+=0.001;
        assert!(prepare_summary(&local,None,&blob,&source,&[9;32],"owner","pi").is_err());
        local=original.clone();local.location_name_start=Some("Library".into());local.gear_runs[0].frames=11;
        assert!(prepare_summary(&local,None,&blob,&source,&[9;32],"owner","pi").is_err());
        local=original.clone();local.location_name_start=Some("Library".into());
        assert!(prepare_summary(&local,None,&blob,&source,&[8;32],"owner","pi").is_err());
        assert!(prepare_summary(&local,None,&blob,&source,&[9;32],"different-owner","pi").is_err());
        assert!(prepare_summary(&local,None,&blob,&source,&[9;32],"owner","different-pi").is_err());
        let mut corrupt=blob.clone();*corrupt.last_mut().unwrap()^=1;
        assert!(prepare_summary(&local,None,&corrupt,&source,&[9;32],"owner","pi").is_err());
        let mut wrong=source.clone();wrong.blob_len+=1;
        assert!(prepare_summary(&local,None,&blob,&wrong,&[9;32],"owner","pi").is_err());
    }
    #[test]
    fn immutable_labels_win_and_existing_summary_labels_are_not_overwritten() {
        let mut original=original_route();original.location_name_start=Some("Original origin".into());original.location_name_end=Some("Original destination".into());
        let (mut source,blob,key,mut before)=encrypted_source(&original);
        let mut local=original.clone();local.points[1][0]+=0.01;local.location_name_start=Some("Different restored label".into());
        before["le"]=Value::String("Existing summary label".into());
        source.summary_ciphertext=Some(crate::encrypt::seal_json_b64(&key,&aad::route_summary("owner","pi",&source.route_id),&before).unwrap());
        let prepared=prepare_summary(&local,None,&blob,&source,&[9;32],"owner","pi").unwrap().unwrap();
        let after:Value=crate::encrypt::open_json_b64(&key,&aad::route_summary("owner","pi",&source.route_id),&prepared.proposal.summary_ciphertext).unwrap();
        assert_eq!(after["ls"],"Original origin");assert_eq!(after["le"],"Existing summary label");
        source.summary_ciphertext=Some(prepared.proposal.summary_ciphertext);
        assert!(prepare_summary(&local,None,&blob,&source,&[9;32],"owner","pi").unwrap().is_none());
    }
    #[test]
    fn unsupported_summary_or_location_versions_do_not_get_downgraded() {
        let original=original_route();let (mut source,blob,key,mut before)=encrypted_source(&original);
        before["v"]=Value::from(5);
        source.summary_ciphertext=Some(crate::encrypt::seal_json_b64(&key,&aad::route_summary("owner","pi",&source.route_id),&before).unwrap());
        assert!(prepare_summary(&original,None,&blob,&source,&[9;32],"owner","pi").is_err());
        before["v"]=Value::from(4);before["lr"]=serde_json::json!({"v":2,"extension":"retained"});
        source.summary_ciphertext=Some(crate::encrypt::seal_json_b64(&key,&aad::route_summary("owner","pi",&source.route_id),&before).unwrap());
        let readings=LocationReadings {v:1,readings:vec![(20000,40.0,-73.0,"Stop".into())]};
        assert!(prepare_summary(&original,Some(&readings),&blob,&source,&[9;32],"owner","pi").unwrap().is_none());
    }
    #[test]
    fn an_unreadable_summary_is_not_treated_as_an_absent_summary() {
        let original=original_route();let (mut source,blob,key,_)=encrypted_source(&original);
        let mut bad=B64.decode(source.summary_ciphertext.as_ref().unwrap()).unwrap();*bad.last_mut().unwrap()^=1;
        source.summary_ciphertext=Some(B64.encode(bad));
        assert!(prepare_summary(&original,None,&blob,&source,&[9;32],"owner","pi").is_err());
        source.summary_ciphertext=None;
        let prepared=prepare_summary(&original,None,&blob,&source,&[9;32],"owner","pi").unwrap().unwrap();
        let after:Value=crate::encrypt::open_json_b64(&key,&aad::route_summary("owner","pi",&source.route_id),&prepared.proposal.summary_ciphertext).unwrap();
        let mut expected=crate::encrypt::route_summary_json(&original);crate::native_metrics::attach(&mut expected,&original);
        assert_eq!(after,expected);
        assert_eq!(prepared.proposal.expected_summary_ciphertext,None);
    }
    #[test]
    fn source_fingerprint_changes_when_restored_data_or_location_evidence_changes() {
        let route=original_route();let base=evidence_fingerprint(&route,None).unwrap();
        let mut changed=route.clone();changed.location_name_start=Some("Library".into());
        assert_ne!(evidence_fingerprint(&changed,None).unwrap(),base);
        let readings=LocationReadings {v:1,readings:vec![(20000,40.0,-73.0,"Stop".into())]};
        assert_ne!(evidence_fingerprint(&route,Some(&readings)).unwrap(),base);
        assert_eq!(evidence_fingerprint(&route,None).unwrap(),base);
    }
    #[test]
    fn missing_nullable_source_fields_do_not_become_authorized_empty_baselines() {
        let original=original_route();let (source,blob,_,_)=encrypted_source(&original);
        let mut encoded=serde_json::to_value(&source).unwrap();encoded.as_object_mut().unwrap().remove("summaryCiphertext");
        assert!(serde_json::from_value::<SummarySource>(encoded).is_err());
        let mut local=original.clone();local.location_name_start=Some("Library".into());
        let prepared=prepare_summary(&local,None,&blob,&source,&[9;32],"owner","pi").unwrap().unwrap();
        let mut encoded=serde_json::to_value(&prepared.proposal).unwrap();encoded.as_object_mut().unwrap().remove("expectedSummaryCiphertext");
        assert!(serde_json::from_value::<SummaryProposal>(encoded).is_err());
    }
    #[test]
    fn shared_browser_documents_match_native_validation() {
        let fixture:serde_json::Value=serde_json::from_str(include_str!("../test-support/location-readings-v1.json")).unwrap();
        for document in fixture["valid"].as_array().unwrap() {
            let decoded:LocationReadings=serde_json::from_value(document.clone()).unwrap();
            assert!(decoded.valid());
            let roundtrip:LocationReadings=serde_json::from_slice(&serde_json::to_vec(&decoded).unwrap()).unwrap();
            assert_eq!(decoded,roundtrip);
        }
        for document in fixture["invalid"].as_array().unwrap() {
            assert!(!serde_json::from_value::<LocationReadings>(document.clone()).unwrap().valid());
        }
    }
    #[test]
    fn reads_only_the_clip_window_without_changing_telemetry() {
        let store = DriveStore::open_memory().unwrap();
        store.with_locked_conn(|db| {
            for (t,name) in [(999,"Before"),(1000,"Origin"),(1020," Library "),(1060,"End"),(1061,"After")] {
                db.execute("INSERT INTO telemetry_samples(ts,source,latitude,longitude,location_name) VALUES(?1,'test',40,-73,?2)",rusqlite::params![t,name]).unwrap();
            }
        });
        let data = for_window(&store,1000).unwrap().unwrap();
        assert_eq!(data.readings.iter().map(|r|(r.0,r.3.as_str())).collect::<Vec<_>>(),vec![(0,"Origin"),(20000,"Library"),(60000,"End")]);
        assert_eq!(store.with_read_conn(|db|db.query_row("SELECT count(*) FROM telemetry_samples",[],|r|r.get::<_,i64>(0))).unwrap(),5);
    }
    #[test]
    fn missing_positions_or_excess_evidence_are_not_silently_removed() {
        let store = DriveStore::open_memory().unwrap();
        store.with_locked_conn(|db| {
            db.execute("INSERT INTO telemetry_samples(ts,source,latitude,longitude,location_name) VALUES(1000,'test',40,-73,'A'),(1010,'test',NULL,NULL,'B')",[]).unwrap();
        });
        assert!(for_window(&store,1000).unwrap().is_none());
        let huge = LocationReadings {v:1,readings:(0..40).map(|i|(i*1000,40.0,-73.0,"x".repeat(100))).collect()};
        assert!(!huge.valid());
    }
    #[test]
    fn invalid_offsets_labels_and_coordinates_are_rejected() {
        let good = LocationReadings {v:1,readings:vec![(0,40.0,-73.0,"École".into()),(60000,40.0,-73.0,"Library".into())]};
        assert!(good.valid());
        for entry in [(-1,40.0,-73.0,"A".into()),(60001,40.0,-73.0,"A".into()),(0,91.0,-73.0,"A".into()),
            (0,0.0,0.0,"A".into()),(0,40.0,-73.0," A".into()),(0,40.0,-73.0,"A\nB".into()),(0,40.0,-73.0,"é".repeat(121))] {
            assert!(!LocationReadings {v:1,readings:vec![entry]}.valid());
        }
        let mut duplicate=good.clone();duplicate.readings[1].0=0;assert!(!duplicate.valid());
        let mut unsupported=good;unsupported.v=2;assert!(!unsupported.valid());
    }
    #[test]
    fn imported_routes_do_not_borrow_the_pis_current_telemetry() {
        let store=DriveStore::open_memory().unwrap();
        let route=Route {source:Some("tessie".into()),file:"2026-09-07_12-00-00.mp4".into(),..Default::default()};
        assert!(for_route(&store,&route).unwrap().is_none());
    }
    #[test]
    fn ordinary_and_edge_parked_clips_do_not_expand_the_encrypted_history_index() {
        use sentryusb_drives::types::GearRun;
        for gears in [vec![1],vec![0,1],vec![1,0],vec![0],vec![]] {
            let route=Route {gear_runs:gears.into_iter().map(|gear|GearRun {gear,frames:10}).collect(),..Default::default()};
            assert!(!has_internal_park(&route));
        }
        let route=Route {gear_runs:vec![GearRun {gear:1,frames:10},GearRun {gear:0,frames:10},GearRun {gear:4,frames:10}],..Default::default()};
        assert!(has_internal_park(&route));
    }

    #[test]
    fn canonical_metric_repair_uses_original_values_and_preserves_legacy_fields_and_key() {
        let original=original_route();let (mut source,blob,key,mut before)=encrypted_source(&original);
        before.as_object_mut().unwrap().remove("nm");
        source.summary_ciphertext=Some(crate::encrypt::seal_json_b64(&key,&aad::route_summary("owner","pi",&source.route_id),&before).unwrap());
        let mut changed_local=original.clone();changed_local.points[1]=[41.0,-74.0];
        let prepared=prepare_summary(&changed_local,None,&blob,&source,&[9;32],"owner","pi").unwrap().unwrap();
        assert_eq!(prepared.proposal.wrapped_route_key,source.wrapped_route_key);
        assert_eq!(prepared.proposal.expected_summary_ciphertext,source.summary_ciphertext);
        let mut after:Value=crate::encrypt::open_json_b64(&key,&aad::route_summary("owner","pi",&source.route_id),&prepared.proposal.summary_ciphertext).unwrap();
        let actual=after.as_object_mut().unwrap().remove("nm").unwrap();
        assert_eq!(actual,serde_json::to_value(crate::native_metrics::for_route(&original).unwrap()).unwrap());
        assert_eq!(after,before);
        source.summary_ciphertext=Some(prepared.proposal.summary_ciphertext);
        assert!(prepare_summary(&changed_local,None,&blob,&source,&[9;32],"owner","pi").unwrap().is_none());
    }
    #[test]
    fn summon_summary_repair_uses_original_runs_and_preserves_existing_envelope() {
        use sentryusb_drives::types::FlagRun;
        let mut original=original_route();
        original.flag_runs=vec![FlagRun {flags:3,frames:60,max_mps:Some(2.0)}];
        let (source,blob,key,before)=encrypted_source(&original);
        let mut local=original.clone();local.flag_runs[0].flags=12;local.flag_runs[0].max_mps=Some(40.0);
        let prepared=prepare_summary(&local,None,&blob,&source,&[9;32],"owner","pi").unwrap().unwrap();
        let mut after:Value=crate::encrypt::open_json_b64(&key,&aad::route_summary("owner","pi",&source.route_id),&prepared.proposal.summary_ciphertext).unwrap();
        assert_eq!(after["se"]["f"],serde_json::json!([[3,60,2.0]]));
        assert_eq!(after["se"]["w"],source.wrapped_route_key);
        assert_eq!(after["se"]["r"],source.route_id);
        assert_eq!(prepared.proposal.wrapped_route_key,source.wrapped_route_key);
        assert_eq!(prepared.proposal.expected_summary_ciphertext,source.summary_ciphertext);
        after.as_object_mut().unwrap().remove("se");assert_eq!(after,before);
        // A local re-extraction cannot invent evidence missing from the original.
        original.flag_runs.clear();let (source,blob,_,_)=encrypted_source(&original);
        assert!(prepare_summary(&local,None,&blob,&source,&[9;32],"owner","pi").unwrap().is_none());
    }
    #[test]
    fn canonical_metric_repair_does_not_downgrade_future_extensions() {
        let original=original_route();let (mut source,blob,key,mut before)=encrypted_source(&original);
        before["nm"]=serde_json::json!({"v":99,"future":"preserve"});
        source.summary_ciphertext=Some(crate::encrypt::seal_json_b64(&key,&aad::route_summary("owner","pi",&source.route_id),&before).unwrap());
        assert!(prepare_summary(&original,None,&blob,&source,&[9;32],"owner","pi").unwrap().is_none());
    }
}
