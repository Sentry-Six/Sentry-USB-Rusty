use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::{Mutex, Notify};
use tracing::warn;

use sentryusb_cloud_crypto::credentials::CloudCredentialsV1;
use sentryusb_drives::DriveStore;
use sentryusb_ws::Hub;

use crate::db_ext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingState {
    Idle,
    Handshaking,
    Polling,
    Complete,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all="snake_case")]
pub enum SyncStage { Credentials, DriveTags, Charging, Rates, Incoming, Home }

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all="camelCase")]
pub struct PendingEdits { pub drive_tags:i64, pub charging:i64, pub rates:i64 }

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all="camelCase")]
pub struct MutableSyncStatus {
    pub running:bool,
    pub home_pending:bool,
    pub failed_stages:Vec<SyncStage>,
    pub pending_edits:Option<PendingEdits>,
    pub last_attempt_at:Option<DateTime<Utc>>,
}

#[derive(Default)]
struct SyncProgress {
    identity:String,
    ticket:u64,
    running:bool,
    home_pending:bool,
    failed_stages:Vec<SyncStage>,
    last_attempt_at:Option<DateTime<Utc>>,
}
fn sync_identity(creds:&CloudCredentialsV1)->anyhow::Result<String> {
    Ok(ring::digest::digest(&ring::digest::SHA256,&serde_json::to_vec(creds)?)
        .as_ref().iter().map(|byte|format!("{byte:02x}")).collect())
}
fn pending_edits(store:&DriveStore)->anyhow::Result<PendingEdits> {
    store.with_read_conn(|conn| {
        let mut counts=PendingEdits::default();
        let mut statement=conn.prepare_cached("SELECT kind,COUNT(*) FROM mutable_dirty GROUP BY kind")?;
        for row in statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?)))? {
            let (kind,count)=row?;
            match kind.as_str() {
                "drive"=>counts.drive_tags=count,
                "charge"=>counts.charging=count,
                "rate"=>counts.rates=count,
                _=>anyhow::bail!("unknown pending mutable kind"),
            }
        }
        Ok(counts)
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct CloudStatus {
    pub paired: bool,
    pub user_id: Option<String>,
    pub pi_id: Option<String>,
    pub paired_at: Option<DateTime<Utc>>,
    pub last_upload_at: Option<DateTime<Utc>>,
    pub last_upload_error: Option<String>,
    pub mutable_sync: MutableSyncStatus,
    pub pending_route_count: i64,
    pub total_uploaded_route_count: i64,
    pub dek_rotation_generation: Option<u32>,
    pub cloud_base_url: String,
    pub pairing_state: PairingState,
    pub pairing_error: Option<String>,

    /// Startup error for an existing credentials file; distinct from never paired.
    pub credentials_load_error: Option<String>,
}

/// Access to charging preferences/Home configuration without an API-crate cycle.
/// Reads must propagate failures. Incoming writes recheck pending local rate
/// edits under the same preferences lock used by local editors.
pub trait RateConfigAccess: Send + Sync {
    /// Only providers with reliable local Home configuration opt into its encrypted mirror.
    fn home_config_sync_enabled(&self)->bool {false}
    /// Read local configuration without turning an I/O/parse failure into
    /// "no Home". Classification and the supported Home-setting mirror are encrypted.
    fn home_geofence(&self)->anyhow::Result<Option<sentryusb_drives::home::HomeGeofence>> {Ok(None)}
    fn load_doc(&self,store:&DriveStore) -> anyhow::Result<serde_json::Value>;
    /// Recover and read under the preferences lock, then durably queue the
    /// existing rate fields only if no local rate edit is already pending.
    fn queue_initial_doc(&self,_store:&DriveStore)->anyhow::Result<()> {
        anyhow::bail!("initial rate publication is unavailable")
    }
    fn store_doc(&self, doc: &serde_json::Value, store: &DriveStore, updated_at_ms: i64) -> anyhow::Result<()>;
    fn confirm_doc(&self, _doc:&serde_json::Value, _store:&DriveStore, _through:i64,
        _receipt:Option<(&str,&str)>) -> anyhow::Result<()> {
        anyhow::bail!("conditional rate confirmation is unavailable")
    }
}

pub struct CloudStateInner {
    pub store: Arc<DriveStore>,
    pub hub: Hub,
    pub notify: Arc<Notify>,
    pub cloud_base_url: String,
    pub credentials_path: String,

    /// `None` disables rate-config sync in tests.
    pub rate_config: Option<Arc<dyn RateConfigAccess>>,

    pub creds: Mutex<Option<CloudCredentialsV1>>,

    pub pairing: Mutex<PairingProgress>,

    pub pairing_cancel: Mutex<Option<Arc<Notify>>>,

    pub last_upload_error: Mutex<Option<String>>,
    sync_progress: Mutex<SyncProgress>,
    pub(crate) home_sync: tokio::sync::Mutex<()>,
    pub(crate) location_sync: Mutex<()>,
    pub(crate) incoming_sync: Mutex<()>,

    pub credentials_load_error: Mutex<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct PairingProgress {
    pub state: PairingState,
    pub error: Option<String>,
}

impl Default for PairingProgress {
    fn default() -> Self {
        PairingProgress { state: PairingState::Idle, error: None }
    }
}

impl CloudStateInner {
    pub fn new(
        store: Arc<DriveStore>,
        hub: Hub,
        notify: Arc<Notify>,
        cloud_base_url: String,
        credentials_path: String,
        rate_config: Option<Arc<dyn RateConfigAccess>>,
    ) -> Self {
        CloudStateInner {
            store,
            hub,
            notify,
            cloud_base_url,
            credentials_path,
            rate_config,
            creds: Mutex::new(None),
            pairing: Mutex::new(PairingProgress::default()),
            pairing_cancel: Mutex::new(None),
            last_upload_error: Mutex::new(None),
            sync_progress: Mutex::new(SyncProgress::default()),
            home_sync: Mutex::new(()),
            location_sync: Mutex::new(()),
            incoming_sync: Mutex::new(()),
            credentials_load_error: Mutex::new(None),
        }
    }

    pub async fn bootstrap_load_credentials(&self) {
        match sentryusb_cloud_crypto::credentials::load(&self.credentials_path) {
            Ok(creds) => {
                let mut guard = self.creds.lock().await;
                *guard = Some(creds);
            }
            Err(e) => {
                // Existing but unreadable credentials require explicit re-pairing.
                if std::path::Path::new(&self.credentials_path).exists() {
                    let msg = format!("{}", e);
                    warn!(
                        "cloud credentials file at `{}` exists but failed to load: {}. \
                         Pi will appear unpaired; user must re-pair.",
                        self.credentials_path, msg,
                    );
                    *self.credentials_load_error.lock().await = Some(msg);
                    self.hub.broadcast(
                        "cloud_status_changed",
                        &serde_json::json!({
                            "paired": false,
                            "reason": "credentials_load_failed",
                        }),
                    );
                }
            }
        }
    }

    pub async fn snapshot_status(&self) -> CloudStatus {
        // Run both counts together on the blocking pool without async guards.
        let store = self.store.clone();
        let (pending_route_count, total_uploaded_route_count, last_upload_secs, pending_edits) =
            tokio::task::spawn_blocking(move || {
                let pending = store.with_read_conn(|conn| {
                    conn.query_row(
                        "SELECT count(*) FROM routes WHERE cloud_uploaded_at IS NULL",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap_or(0)
                });
                let (total, last) = db_ext::upload_summary(&store);
                (pending, total, last, pending_edits(&store).ok())
            })
            .await
            .unwrap_or((0, 0, None, None));
        let last_upload_at = last_upload_secs
            .and_then(|s| chrono::DateTime::<Utc>::from_timestamp(s, 0));

        let creds_guard = self.creds.lock().await;
        let pairing_guard = self.pairing.lock().await;
        let last_upload_error = self.last_upload_error.lock().await.clone();
        let credentials_load_error = self.credentials_load_error.lock().await.clone();

        let progress=self.sync_progress.lock().await;
        let current=creds_guard.as_ref().and_then(|creds|sync_identity(creds).ok());
        let mut mutable_sync=MutableSyncStatus {pending_edits,..Default::default()};
        if current.as_ref()==Some(&progress.identity) {
            mutable_sync.running=progress.running;
            mutable_sync.home_pending=progress.home_pending;
            mutable_sync.failed_stages=progress.failed_stages.clone();
            mutable_sync.last_attempt_at=progress.last_attempt_at;
        }
        drop(progress);
        match creds_guard.as_ref() {
            Some(c) => CloudStatus {
                paired: true,
                user_id: Some(c.user_id.clone()),
                pi_id: Some(c.pi_id.clone()),
                paired_at: Some(c.paired_at),
                last_upload_at,
                last_upload_error,
                mutable_sync,
                pending_route_count,
                total_uploaded_route_count,
                dek_rotation_generation: Some(c.dek_rotation_generation),
                cloud_base_url: c.cloud_base_url.clone(),
                pairing_state: pairing_guard.state,
                pairing_error: pairing_guard.error.clone(),
                credentials_load_error: None,
            },
            None => CloudStatus {
                paired: false,
                user_id: None,
                pi_id: None,
                paired_at: None,
                last_upload_at,
                last_upload_error,
                mutable_sync,
                pending_route_count,
                total_uploaded_route_count,
                dek_rotation_generation: None,
                cloud_base_url: self.cloud_base_url.clone(),
                pairing_state: pairing_guard.state,
                pairing_error: pairing_guard.error.clone(),
                credentials_load_error,
            },
        }
    }

    pub(crate) async fn begin_sync(&self,expected:&CloudCredentialsV1)->anyhow::Result<u64> {
        let _creds=self.current_credentials(expected).await?;
        let mut progress=self.sync_progress.lock().await;
        progress.ticket=progress.ticket.checked_add(1).ok_or_else(||anyhow::anyhow!("sync status sequence exhausted"))?;
        let identity=sync_identity(expected)?;
        if progress.identity!=identity {
            progress.failed_stages.clear();
            progress.home_pending=false;
            progress.last_attempt_at=None;
        }
        progress.identity=identity;
        progress.running=true;
        let ticket=progress.ticket;
        drop(progress);
        self.hub.broadcast("cloud_sync_changed",&serde_json::json!({"running":true}));
        Ok(ticket)
    }

    pub(crate) async fn finish_sync(&self,expected:&CloudCredentialsV1,ticket:u64,failed:Vec<SyncStage>)->anyhow::Result<bool> {
        self.finish_sync_with_home(expected,ticket,failed,false).await
    }
    pub(crate) async fn finish_sync_with_home(&self,expected:&CloudCredentialsV1,ticket:u64,failed:Vec<SyncStage>,home_pending:bool)->anyhow::Result<bool> {
        let _creds=self.current_credentials(expected).await?;
        let mut progress=self.sync_progress.lock().await;
        if progress.ticket!=ticket || progress.identity!=sync_identity(expected)? {return Ok(false)}
        progress.running=false;
        progress.home_pending=home_pending;
        progress.failed_stages=failed;
        progress.last_attempt_at=Some(Utc::now());
        drop(progress);
        self.hub.broadcast("cloud_sync_changed",&serde_json::json!({"running":false}));
        Ok(true)
    }

    pub(crate) async fn begin_pairing(&self)->anyhow::Result<Arc<Notify>> {
        let creds=self.creds.lock().await;
        anyhow::ensure!(creds.is_none(),"already paired; unpair first");
        let mut active=self.pairing_cancel.lock().await;
        anyhow::ensure!(active.is_none(),"pairing is already in progress");
        let attempt=Arc::new(Notify::new());
        *active=Some(attempt.clone());
        *self.pairing.lock().await=PairingProgress {state:PairingState::Handshaking,error:None};
        self.hub.broadcast("cloud_status_changed",&serde_json::json!({"pairingState":"handshaking"}));
        Ok(attempt)
    }

    pub(crate) async fn pairing_progress(&self,attempt:&Arc<Notify>,phase:PairingState)->anyhow::Result<()> {
        let creds=self.creds.lock().await;
        anyhow::ensure!(creds.is_none(),"pairing has already completed");
        let active=self.pairing_cancel.lock().await;
        anyhow::ensure!(active.as_ref().is_some_and(|current|Arc::ptr_eq(current,attempt)),"pairing cancelled or replaced");
        anyhow::ensure!(matches!(phase,PairingState::Handshaking|PairingState::Polling),"invalid active pairing phase");
        let mut progress=self.pairing.lock().await;
        if progress.state!=phase || progress.error.is_some() {
            *progress=PairingProgress {state:phase,error:None};
            self.hub.broadcast("cloud_status_changed",&serde_json::json!({"pairingState":phase}));
        }
        Ok(())
    }

    pub(crate) async fn complete_pairing(&self,attempt:&Arc<Notify>,creds:CloudCredentialsV1)->anyhow::Result<()> {
        let mut current=self.creds.lock().await;
        let mut active=self.pairing_cancel.lock().await;
        anyhow::ensure!(current.is_none() && active.as_ref().is_some_and(|current|Arc::ptr_eq(current,attempt)),
            "pairing cancelled, replaced or already completed");
        sentryusb_cloud_crypto::credentials::save_atomic(&self.credentials_path,&creds)?;
        *current=Some(creds);
        *active=None;
        *self.pairing.lock().await=PairingProgress {state:PairingState::Complete,error:None};
        *self.credentials_load_error.lock().await=None;
        *self.last_upload_error.lock().await=None;
        self.hub.broadcast("cloud_status_changed",&serde_json::json!({"paired":true,"pairingState":"complete"}));
        Ok(())
    }

    pub(crate) async fn fail_pairing(&self,attempt:&Arc<Notify>) {
        let mut active=self.pairing_cancel.lock().await;
        if !active.as_ref().is_some_and(|current|Arc::ptr_eq(current,attempt)) {return}
        *active=None;
        *self.pairing.lock().await=PairingProgress {state:PairingState::Error,error:Some("Pairing failed. Try a new code.".into())};
        self.hub.broadcast("cloud_status_changed",&serde_json::json!({"pairingState":"error"}));
    }

    pub async fn cancel_pairing(&self) {
        let mut active=self.pairing_cancel.lock().await;
        if let Some(attempt)=active.take() {
            // One pending permit also covers cancellation just before the
            // network future starts listening; notify_waiters would lose it.
            attempt.notify_one();
            *self.pairing.lock().await=PairingProgress {state:PairingState::Idle,error:Some("cancelled".into())};
            self.hub.broadcast("cloud_status_changed",&serde_json::json!({"pairingState":"idle"}));
        }
    }

    pub async fn unpair(&self) -> anyhow::Result<()> {
        let mut creds_guard = self.creds.lock().await;
        // Prevent an in-flight pairing response from undoing this unpair.
        self.cancel_pairing().await;
        if creds_guard.is_some() {

            sentryusb_cloud_crypto::credentials::secure_delete(&self.credentials_path)?;
        }
        *creds_guard = None;
        drop(creds_guard);

        *self.last_upload_error.lock().await = None;
        *self.credentials_load_error.lock().await = None;

        self.hub.broadcast(
            "cloud_status_changed",
            &serde_json::json!({ "paired": false }),
        );
        Ok(())
    }

    pub async fn set_credentials(&self, new_creds: CloudCredentialsV1) -> anyhow::Result<()> {
        let mut guard = self.creds.lock().await;
        sentryusb_cloud_crypto::credentials::save_atomic(&self.credentials_path, &new_creds)?;
        *guard = Some(new_creds);
        drop(guard);
        *self.credentials_load_error.lock().await = None;
        self.hub.broadcast(
            "cloud_status_changed",
            &serde_json::json!({ "paired": true }),
        );
        Ok(())
    }

    pub(crate) async fn replace_credentials_if_current(
        &self, expected: &CloudCredentialsV1, updated: CloudCredentialsV1,
    ) -> anyhow::Result<bool> {
        let mut guard=self.creds.lock().await;
        if guard.as_ref()!=Some(expected) {return Ok(false)}
        sentryusb_cloud_crypto::credentials::save_atomic(&self.credentials_path,&updated)?;
        *guard=Some(updated);drop(guard);
        *self.credentials_load_error.lock().await=None;
        self.hub.broadcast("cloud_status_changed",&serde_json::json!({"paired":true}));
        Ok(true)
    }

    pub(crate) async fn current_credentials<'a>(&'a self, expected: &CloudCredentialsV1)
        -> anyhow::Result<tokio::sync::MutexGuard<'a, Option<CloudCredentialsV1>>> {
        let guard=self.creds.lock().await;
        anyhow::ensure!(guard.as_ref()==Some(expected),"Cloud pairing changed during request");
        Ok(guard)
    }

    pub async fn handle_remote_revoke(&self, expected: &CloudCredentialsV1) -> bool {
        let mut guard = self.creds.lock().await;
        if guard.as_ref()!=Some(expected) {
            return false;
        }
        if let Err(e) =
            sentryusb_cloud_crypto::credentials::secure_delete(&self.credentials_path)
        {
            warn!("remote revoke: secure_delete failed: {}", e);
        }
        *guard = None;
        drop(guard);

        *self.last_upload_error.lock().await = Some("revoked".to_string());
        *self.credentials_load_error.lock().await = None;

        self.hub.broadcast(
            "cloud_status_changed",
            &serde_json::json!({ "paired": false, "reason": "revoked" }),
        );
        true
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path="sync_status_tests.rs"]
mod sync_status_tests;
