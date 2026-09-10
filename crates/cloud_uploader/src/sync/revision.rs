use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD}};
use serde::{Deserialize, Deserializer, Serialize};
use sentryusb_cloud_crypto::credentials::CloudCredentialsV1;
use sentryusb_drives::schema;

use crate::{client::CloudClient, state::CloudStateInner};
use super::{ChangesResponse, ChargeChange, RateConfigChange, RouteChange};

const CHECKPOINT_KEY: &str = "cloud_mutable_revision_checkpoint_v2";
const WALK_KEY: &str = "cloud_mutable_revision_walk_v2";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

// A missing nullable field is malformed, not permission to clear local data.
fn nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where D: Deserializer<'de>, T: Deserialize<'de> {
    Option::<T>::deserialize(deserializer)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    ok: bool,
    items: Vec<Item>,
    #[serde(deserialize_with = "nullable")]
    next_cursor: Option<String>,
    sync: Revision,
}

#[derive(Deserialize)]
struct Revision {
    version: u8,
    revision: String,
}

#[derive(Clone, Copy, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "camelCase")]
pub(super) enum Kind { Charge, RateConfig, Route }
impl Kind {
    pub(super) fn name(self)->&'static str {
        match self {Self::Charge=>"charge",Self::RateConfig=>"rateConfig",Self::Route=>"route"}
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Item {
    pub(super) kind: Kind,
    pub(super) id: String,
    pub(super) revision: String,
    pub(super) updated_at_ms: i64,
    #[serde(deserialize_with = "nullable")]
    pub(super) ciphertext: Option<String>,
    pub(super) wrapped_key: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Checkpoint {
    version: u8,
    binding: String,
    revision: String,
}

fn revision(value: &str) -> Result<i64> {
    ensure!(!value.is_empty() && value.len() <= 19 && value.bytes().all(|b| b.is_ascii_digit())
        && (value == "0" || !value.starts_with('0')), "invalid mutable revision");
    value.parse::<i64>().context("mutable revision out of range")
}

// Bind persisted progress and late responses to the complete pairing identity,
// including endpoint, wrapped key and token generation. No key/token is stored
// in the checkpoint; the fingerprint is only a local equality check.
pub(super) fn binding(creds: &CloudCredentialsV1) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, &serde_json::to_vec(creds)?)))
}

pub(super) async fn current_pairing<'a>(state: &'a CloudStateInner, expected: &str)
    -> Result<tokio::sync::MutexGuard<'a, Option<CloudCredentialsV1>>> {
    let guard = state.creds.lock().await;
    let current = guard.as_ref().context("pairing ended during mutable sync")?;
    ensure!(binding(current)? == expected, "pairing changed during mutable sync");
    Ok(guard)
}

pub(super) fn valid_envelope(value: &str, max: usize, exact: Option<usize>) -> Result<()> {
    ensure!(value.len() <= ((max + 2) / 3) * 4, "mutable envelope too large");
    let bytes = B64.decode(value).context("invalid mutable envelope encoding")?;
    ensure!(bytes.len() >= 31 && bytes.len() <= max && bytes[0] == 1
        && exact.is_none_or(|len| bytes.len() == len) && B64.encode(&bytes) == value,
        "invalid mutable envelope framing");
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
struct Walk {
    since: Option<String>,
    checkpoint: Option<String>,
    cursor: Option<String>,
    last: Option<(i64, Kind, String)>,
}

#[derive(Serialize, Deserialize)]
struct SavedWalk {
    version: u8,
    binding: String,
    base: Option<String>,
    walk: Walk,
}

fn resume_valid(walk: &Walk, pi_id: &str) -> bool {
    let Some(cursor)=walk.cursor.as_ref() else {return false};
    if cursor.is_empty() || cursor.len()>2048 || !cursor.bytes().all(|b|b.is_ascii_alphanumeric() || b==b'_' || b==b'-') {return false}
    let Some(checkpoint)=walk.checkpoint.as_deref().and_then(|value|revision(value).ok()) else {return false};
    let Some((at,kind,id))=walk.last.as_ref() else {return false};
    if *kind==Kind::RateConfig {
        if id!=pi_id {return false}
    } else if id.len()!=64 || !id.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {return false}
    match walk.since.as_deref() {
        Some(value)=>revision(value).is_ok_and(|since| since<=checkpoint && *at>since && *at<=checkpoint),
        None=>*at==0,
    }
}

// Both the completed checkpoint and the in-progress cursor are compare-and-swap
// inputs. A late response must be rejected before it can overwrite local values.
pub(super) fn check_progress(state: &CloudStateInner, initial: &Option<String>, saved: &Option<String>) -> Result<()> {
    state.store.with_locked_conn(|conn| {
        ensure!(schema::meta_get(conn,CHECKPOINT_KEY)?==*initial && schema::meta_get(conn,WALK_KEY)?==*saved,
            "another mutable walk advanced or requested a rescan");
        Ok(())
    })
}

impl Walk {
    fn new(since: Option<String>) -> Self {
        Self { since, checkpoint: None, cursor: None, last: None }
    }

    fn path(&self) -> String {
        let mut path = "/api/pi/sync/changes/v2?limit=200".to_string();
        if let Some(since) = &self.since { path.push_str(&format!("&since={since}")); }
        // Validated base64url alphabet requires no extra URL escaping.
        if let Some(cursor) = &self.cursor { path.push_str(&format!("&cursor={cursor}")); }
        path
    }

    fn validate(&mut self, page: &Page, pi_id: &str) -> Result<()> {
        ensure!(page.ok && page.sync.version == 2 && page.items.len() <= 200, "invalid mutable page");
        let checkpoint = revision(&page.sync.revision)?;
        if let Some(since) = &self.since { ensure!(checkpoint >= revision(since)?, "mutable checkpoint regressed"); }
        if let Some(previous) = &self.checkpoint {
            ensure!(previous == &page.sync.revision, "mutable checkpoint changed within walk");
        }
        if let Some(cursor) = &page.next_cursor {
            ensure!(!page.items.is_empty() && !cursor.is_empty() && cursor.len() <= 2048
                && cursor.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                && self.cursor.as_ref() != Some(cursor), "invalid mutable page cursor");
        }
        let mut last = self.last.clone();
        for item in &page.items {
            let at = revision(&item.revision)?;
            ensure!((0..=8_640_000_000_000_000).contains(&item.updated_at_ms), "invalid mutable timestamp");
            if let Some(since) = &self.since {
                ensure!(at > revision(since)? && at <= checkpoint, "mutable item outside checkpoint");
            }
            let order = (if self.since.is_some() { at } else { 0 }, item.kind, item.id.clone());
            ensure!(last.as_ref().is_none_or(|last| last < &order), "mutable page order did not advance");
            last = Some(order);
            if item.kind == Kind::RateConfig {
                ensure!(item.id == pi_id && item.wrapped_key.is_none(), "foreign mutable rate configuration");
                valid_envelope(item.ciphertext.as_deref().context("missing rate configuration")?, 16384, None)?;
            } else {
                ensure!(item.id.len() == 64 && item.id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                    "invalid mutable record id");
                valid_envelope(item.wrapped_key.as_deref().context("missing wrapped mutable key")?, 61, Some(61))?;
                if let Some(ciphertext) = &item.ciphertext { valid_envelope(ciphertext, 2048, None)?; }
            }
        }
        self.checkpoint = Some(page.sync.revision.clone());
        self.cursor = page.next_cursor.clone();
        self.last = last;
        Ok(())
    }
}

pub(super) fn changes(items: Vec<Item>) -> ChangesResponse {
    let mut changes = ChangesResponse { routes: Vec::new(), charges: Vec::new(), rate_config: None };
    for item in items {
        match item.kind {
            Kind::Route => changes.routes.push(RouteChange { route_id: item.id }),
            Kind::Charge => changes.charges.push(ChargeChange { charge_id: item.id,
                mutable_ciphertext: item.ciphertext, wrapped_charge_key: item.wrapped_key.unwrap(), updated_at_ms: item.updated_at_ms }),
            Kind::RateConfig => changes.rate_config = Some(RateConfigChange { ciphertext: item.ciphertext, updated_at_ms: item.updated_at_ms }),
        }
    }
    changes
}

// Construct a marker left by the legacy timestamp writer. Reader tests keep
// recovery coverage for installations upgrading with this marker still saved.
#[cfg(test)]
pub(super) fn require_full_read(state: &CloudStateInner, expected: &str) -> Result<()> {
    use ring::rand::SecureRandom;
    let mut nonce = [0; 16];
    ring::rand::SystemRandom::new().fill(&mut nonce).map_err(|_| anyhow::anyhow!("rescan nonce failed"))?;
    let marker = serde_json::json!({ "version": 2, "binding": expected, "rescan": true,
        "request": URL_SAFE_NO_PAD.encode(nonce) }).to_string();
    state.store.with_locked_conn(|conn| schema::meta_set(conn, CHECKPOINT_KEY, &marker))?;
    Ok(())
}

pub(super) async fn bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>> {
    ensure!(response.content_length().is_none_or(|n| n <= MAX_RESPONSE_BYTES as u64), "mutable response too large");
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(bytes.len() + chunk.len() <= MAX_RESPONSE_BYTES, "mutable response too large");
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(super) fn progress(state:&CloudStateInner)->Result<(Option<String>,Option<String>)> {
    state.store.with_locked_conn(|conn|Ok((schema::meta_get(conn,CHECKPOINT_KEY)?,schema::meta_get(conn,WALK_KEY)?)))
}

pub(super) async fn pull_changes(
    state:&Arc<CloudStateInner>,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32],
)->Result<()> {
    let _lease=state.incoming_sync.lock().await;
    let retried=super::incoming_apply::retry(state,client,creds,pi_key).await;
    let received=pull_feed(state,client,creds,pi_key).await;
    received?;
    let expected=binding(creds)?;
    let _guard=current_pairing(state,&expected).await?;
    let remaining=state.store.with_locked_conn(|conn|super::incoming_retry::count(conn,&expected))?;
    if remaining>0 {
        retried?;
        anyhow::bail!("some incoming changes remain saved for retry");
    }
    Ok(())
}

async fn pull_feed(
    state: &Arc<CloudStateInner>, client: &CloudClient,
    creds: &CloudCredentialsV1, pi_key: &[u8; 32],
) -> Result<()> {
    let expected = binding(creds)?;
    let (initial, mut saved) = {
        let _guard = current_pairing(state, &expected).await?;
        state.store.with_locked_conn(|conn| -> Result<_> {
            Ok((schema::meta_get(conn,CHECKPOINT_KEY)?,schema::meta_get(conn,WALK_KEY)?))
        })?
    };
    let since = initial.as_deref().and_then(|raw| serde_json::from_str::<Checkpoint>(raw).ok())
        .filter(|value| value.version == 2 && value.binding == expected && revision(&value.revision).is_ok())
        .map(|value| value.revision);
    let mut walk = saved.as_deref().and_then(|raw|serde_json::from_str::<SavedWalk>(raw).ok())
        .filter(|resume| resume.version==1 && resume.binding==expected && resume.base==initial
            && (resume.walk.since.is_none() || resume.walk.since==since) && resume_valid(&resume.walk,&creds.pi_id))
        .map(|resume|resume.walk).unwrap_or_else(||Walk::new(since));
    let mut reset = false;
    loop {
        { let _guard = current_pairing(state, &expected).await?; }
        let response = client.get_bearer(&walk.path()).await.context("read mutable revision page")?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            let body: serde_json::Value = serde_json::from_slice(&bounded_body(response).await?)?;
            if !reset && body.get("error").and_then(|v| v.as_str()) == Some("sync_reset_required") {
                let _guard=current_pairing(state,&expected).await?;
                state.store.with_locked_conn(|conn| -> Result<()> {
                    let tx=conn.unchecked_transaction()?;
                    ensure!(schema::meta_get(&tx,CHECKPOINT_KEY)?==initial && schema::meta_get(&tx,WALK_KEY)?==saved,
                        "mutable progress changed during reset");
                    schema::meta_del(&tx,WALK_KEY)?;tx.commit()?;Ok(())
                })?;
                saved=None;walk=Walk::new(None);reset=true;
                continue;
            }
            bail!("mutable revision reset failed");
        }
        let response = CloudClient::classify(response).await.context("mutable revision feed unavailable")?;
        let page: Page = serde_json::from_slice(&bounded_body(response).await?).context("decode mutable revision page")?;
        walk.validate(&page, &creds.pi_id)?;
        let context=super::incoming_apply::Context {progress:(initial.clone(),saved.clone()),retry_generation:None};
        let (done,failed)=super::incoming_apply::apply(state,client,creds,pi_key,page.items,&context).await?;
        let _guard=current_pairing(state,&expected).await?;
        check_progress(state,&initial,&saved)?;
        if walk.cursor.is_none() {
            let checkpoint = serde_json::to_string(&Checkpoint { version: 2, binding: expected.clone(),
                revision: walk.checkpoint.context("missing mutable checkpoint")? })?;
            state.store.with_locked_conn(|conn| -> Result<()> {
                let tx = conn.unchecked_transaction()?;
                ensure!(schema::meta_get(&tx,CHECKPOINT_KEY)?==initial && schema::meta_get(&tx,WALK_KEY)?==saved,
                    "another mutable walk advanced");
                super::incoming_retry::record(&tx,&expected,&failed,&done)?;
                schema::meta_set(&tx, CHECKPOINT_KEY, &checkpoint)?;
                schema::meta_del(&tx,WALK_KEY)?;
                tx.commit()?;
                Ok(())
            })?;
            return Ok(());
        }
        // Cursor advancement and failed-record refresh requests commit together.
        // Readable records keep moving; unresolved records are never discarded.
        let next=serde_json::to_string(&SavedWalk {version:1,binding:expected.clone(),base:initial.clone(),walk:walk.clone()})?;
        state.store.with_locked_conn(|conn| -> Result<()> {
            let tx=conn.unchecked_transaction()?;
            ensure!(schema::meta_get(&tx,CHECKPOINT_KEY)?==initial && schema::meta_get(&tx,WALK_KEY)?==saved,
                "mutable progress changed before saving cursor");
            super::incoming_retry::record(&tx,&expected,&failed,&done)?;
            schema::meta_set(&tx,WALK_KEY,&next)?;tx.commit()?;Ok(())
        })?;
        saved=Some(next);
    }
}

#[cfg(test)]
#[path = "revision_tests.rs"]
pub(super) mod tests;
