//! Local edit operations persisted with the sync queue. The uploader applies
//! them to freshly decrypted Cloud documents before encrypting the result.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const RATE_KEYS:&[&str]=&["charging_currency","charging_default_rate","charging_tag_rates"];
pub const RATE_PREFERENCES_JOURNAL:&str="cloud_rate_preferences_publication_v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "field", rename_all = "camelCase")]
pub enum Edit {
    Tags { added: Vec<String>, removed: Vec<String> },
    CostOverride { before: Value, after: Value },
    RateConfig {
        before: Value, after: Value, force: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        required_tag: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Intent {
    pub legacy: bool,
    pub edits: Vec<(i64, Edit)>,
}

pub fn tag_edit(before: &[String], after: &[String]) -> Edit {
    let before: BTreeSet<_> = before.iter().filter(|s| !s.is_empty()).cloned().collect();
    let after: BTreeSet<_> = after.iter().filter(|s| !s.is_empty()).cloned().collect();
    Edit::Tags { added: after.difference(&before).cloned().collect(), removed: before.difference(&after).cloned().collect() }
}

pub fn cost_value(cost: &Option<(f64, String)>) -> Result<Value> {
    match cost {
        None => Ok(Value::Null),
        Some((amount, currency)) => {
            ensure!(amount.is_finite() && *amount >= 0.0, "invalid cost intent");
            Ok(serde_json::json!({"amount":amount,"currency":currency}))
        }
    }
}

// Call after creating/updating the parent queue row, in the same transaction.
// Existing untracked queues remain explicitly legacy; their lost field history
// cannot be reconstructed safely from the current full document alone.
pub(crate) fn record(conn: &Connection, kind: &str, key: &str, prior_generation: Option<i64>, edit: &Edit) -> Result<()> {
    conn.execute("INSERT OR IGNORE INTO mutable_intent_state(kind,key,legacy_through) VALUES(?1,?2,?3)",
        params![kind,key,prior_generation])?;
    let generation: i64 = conn.query_row("SELECT changed_at FROM mutable_dirty WHERE kind=?1 AND key=?2",
        params![kind,key], |row| row.get(0))?;
    conn.execute("INSERT INTO mutable_intent_events(kind,key,changed_at,payload) VALUES(?1,?2,?3,?4)",
        params![kind,key,generation,serde_json::to_string(edit)?])?;
    Ok(())
}

pub(crate) fn queued(conn: &Connection, kind: &str, key: &str) -> Result<Option<i64>> {
    Ok(conn.query_row("SELECT changed_at FROM mutable_dirty WHERE kind=?1 AND key=?2",
        params![kind,key], |row| row.get(0)).optional()?)
}

pub fn read(conn: &Connection, kind: &str, key: &str) -> Result<Intent> {
    let legacy: Option<Option<i64>> = conn.query_row("SELECT legacy_through FROM mutable_intent_state WHERE kind=?1 AND key=?2",
        params![kind,key], |row| row.get(0)).optional()?;
    let mut statement = conn.prepare_cached("SELECT changed_at,payload FROM mutable_intent_events WHERE kind=?1 AND key=?2 ORDER BY changed_at")?;
    let mut edits = Vec::new();
    for row in statement.query_map(params![kind,key], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?)))? {
        let (at, payload) = row?;
        edits.push((at, serde_json::from_str(&payload).context("invalid queued mutable intent")?));
    }
    Ok(Intent { legacy: legacy.map(|through| through.is_some()).unwrap_or(true), edits })
}

pub(crate) fn retire(conn: &Connection, kind: &str, key: &str, through: i64) -> Result<()> {
    conn.execute("DELETE FROM mutable_intent_events WHERE kind=?1 AND key=?2 AND changed_at<=?3", params![kind,key,through])?;
    conn.execute("UPDATE mutable_intent_state SET legacy_through=NULL WHERE kind=?1 AND key=?2 AND legacy_through<=?3", params![kind,key,through])?;
    Ok(())
}
