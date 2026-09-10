//! Advertise the installed reader/writer before publishing scoped drive tags.
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::json;
use sentryusb_cloud_crypto::credentials::CloudCredentialsV1;

use crate::{client::CloudClient, state::CloudStateInner};
use super::revision;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Confirmation {
    ok: bool,
    scoped_tags_version: u8,
}

pub(super) async fn register(state: &CloudStateInner, client: &CloudClient,
    credentials: &CloudCredentialsV1) -> Result<()> {
    let binding = revision::binding(credentials)?;
    { let _guard = revision::current_pairing(state, &binding).await?; }
    // The server handles repeats without another write/audit. Recheck each
    // sweep so a restored server cannot leave a stale local capability cache.
    let response = client.post_json_bearer("/api/pi/sync/capabilities", &json!({
        "piId": credentials.pi_id,
        "dekRotationGeneration": credentials.dek_rotation_generation,
        "scopedTagsVersion": 2,
    })).await?;
    ensure!(response.status().is_success(), "Cloud drive-tag capability was not confirmed ({})", response.status().as_u16());
    let confirmation: Confirmation = serde_json::from_slice(&revision::bounded_body(response).await?)?;
    ensure!(confirmation.ok && confirmation.scoped_tags_version == 2,
        "Cloud did not confirm scoped drive tags");
    let _guard = revision::current_pairing(state, &binding).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{conditional_charge::tests::server, revision::tests::fixture};

    #[tokio::test]
    async fn each_sweep_confirms_the_current_device_without_changing_local_data() {
        let (state, credentials) = fixture();
        let (client, task) = server(2, |path, body, _| {
            assert_eq!(path, "/api/pi/sync/capabilities");
            assert_eq!(body, json!({"piId":"pi","dekRotationGeneration":0,"scopedTagsVersion":2}));
            (200, json!({"ok":true,"scopedTagsVersion":2}))
        }).await;
        for _ in 0..2 { register(&state, &client, &credentials).await.unwrap(); }
        task.await.unwrap();
        assert!(state.store.dirty_mutables().unwrap().is_empty());
        assert_eq!(state.creds.lock().await.as_ref().unwrap().pi_auth_token, credentials.pi_auth_token);
    }

    #[tokio::test]
    async fn errors_or_unsupported_confirmations_cannot_enable_publication() {
        for (status, body) in [
            (503, json!({"error":"pi_mutable_writes_paused"})),
            (401, json!({"error":"unknown_token"})),
            (409, json!({"error":"pi_key_stale"})),
            (200, json!({"ok":false,"scopedTagsVersion":2})),
            (200, json!({"ok":true,"scopedTagsVersion":1})),
            (200, json!({"ok":true,"scopedTagsVersion":"2"})),
            (200, json!({"ok":true})),
        ] {
            let (state, credentials) = fixture();
            let (client, task) = server(1, move |_, _, _| (status, body.clone())).await;
            assert!(register(&state, &client, &credentials).await.is_err());
            task.await.unwrap();
            assert!(state.store.dirty_mutables().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn changed_pairing_after_confirmation_cannot_start_the_old_writer() {
        let (state, credentials) = fixture(); let changed = state.clone();
        let (client, task) = server(1, move |_, _, _| {
            changed.creds.try_lock().unwrap().as_mut().unwrap().pi_auth_token = "replacement".into();
            (200, json!({"ok":true,"scopedTagsVersion":2}))
        }).await;
        let error = register(&state, &client, &credentials).await.unwrap_err();
        task.await.unwrap();
        assert!(error.to_string().contains("pairing changed"));
    }

    #[tokio::test]
    async fn ended_pairing_is_rejected_before_contacting_cloud() {
        let (state, credentials) = fixture(); *state.creds.lock().await = None;
        let client = CloudClient::new("http://127.0.0.1:1").with_bearer(&[3;32]);
        let error = register(&state, &client, &credentials).await.unwrap_err();
        assert!(error.to_string().contains("pairing ended"));
    }

    #[tokio::test]
    async fn uncertain_confirmation_retries_instead_of_remembering_success() {
        let (state, credentials) = fixture();
        let (client, task) = server(2, |_, _, index| if index == 0 {
            (503, json!({"error":"response_lost"}))
        } else { (200, json!({"ok":true,"scopedTagsVersion":2})) }).await;
        assert!(register(&state, &client, &credentials).await.is_err());
        register(&state, &client, &credentials).await.unwrap();
        task.await.unwrap();
    }
}
