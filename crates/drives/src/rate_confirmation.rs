//! Confirm Cloud rates only after their local preferences file is durable.
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use crate::{DriveStore, mutable_intent::{self, Intent, RATE_PREFERENCES_JOURNAL}, schema};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub generation: i64,
    pub intent: Intent,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Confirmation {
    pub through: i64,
    pub snapshot: Snapshot,
    pub receipt: Option<(String, String)>,
}
fn snapshot(conn: &Connection) -> Result<Snapshot> {
    Ok(Snapshot {
        generation: mutable_intent::queued(conn, "rate", "")?.context("rate edit is no longer queued")?,
        intent: mutable_intent::read(conn, "rate", "")?,
    })
}
fn validate(conn: &Connection, confirmation: &Confirmation) -> Result<()> {
    ensure!(confirmation.through > 0 && confirmation.through <= confirmation.snapshot.generation,
        "invalid rate confirmation generation");
    ensure!(snapshot(conn)? == confirmation.snapshot, "rate intent changed before confirmation");
    ensure!(!confirmation.snapshot.intent.legacy || confirmation.through == confirmation.snapshot.generation,
        "newer untracked rate edits cannot be replayed");
    if let Some((key, raw)) = &confirmation.receipt {
        let suffix = key.strip_prefix("cloud_rate_publication_v3:").context("invalid rate receipt key")?;
        let binding = URL_SAFE_NO_PAD.decode(suffix).context("invalid rate receipt binding")?;
        ensure!(binding.len() == 32 && URL_SAFE_NO_PAD.encode(&binding) == suffix, "invalid rate receipt binding");
        ensure!(schema::meta_get(conn, key)?.as_deref() == Some(raw), "rate publication receipt changed");
    }
    Ok(())
}

impl DriveStore {
    pub fn rate_sync_snapshot(&self) -> Result<Snapshot> {
        self.with_locked_conn(snapshot)
    }
    /// The caller holds the preferences lock and has recovered earlier file work.
    pub fn stage_rate_confirmation(&self, confirmation: &Confirmation, journal: &str) -> Result<()> {
        self.with_durable_conn(|conn| {
            let tx = conn.transaction()?;
            validate(&tx, confirmation)?;
            ensure!(schema::meta_get(&tx, RATE_PREFERENCES_JOURNAL)?.is_none(), "rate file publication is already pending");
            schema::meta_set(&tx, RATE_PREFERENCES_JOURNAL, journal)?;
            tx.commit()?;
            Ok(())
        })
    }
    /// Recovery checks these guards before touching the preferences file.
    pub fn check_rate_confirmation(&self, confirmation: &Confirmation, journal: &str) -> Result<()> {
        self.with_locked_conn(|conn| {
            validate(conn, confirmation)?;
            ensure!(schema::meta_get(conn, RATE_PREFERENCES_JOURNAL)?.as_deref() == Some(journal), "rate file publication changed");
            Ok(())
        })
    }
    /// Invoke only after the file and its containing directory have been flushed.
    pub fn finish_rate_confirmation(&self, confirmation: &Confirmation, journal: &str) -> Result<()> {
        self.with_durable_conn(|conn| {
            let tx = conn.transaction()?;
            validate(&tx, confirmation)?;
            ensure!(schema::meta_get(&tx, RATE_PREFERENCES_JOURNAL)?.as_deref() == Some(journal), "rate file publication changed");
            mutable_intent::retire(&tx, "rate", "", confirmation.through)?;
            tx.execute("DELETE FROM mutable_dirty WHERE kind='rate' AND key='' AND changed_at<=?1", params![confirmation.through])?;
            if let Some((key, _)) = &confirmation.receipt { schema::meta_del(&tx, key)?; }
            schema::meta_del(&tx, RATE_PREFERENCES_JOURNAL)?;
            tx.commit()?;
            Ok(())
        })
    }
}
