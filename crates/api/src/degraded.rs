//! Router served when the drive DB cannot be opened on a real device.
//!
//! Database-dependent endpoints return `503 database_unavailable`; an
//! authenticated allowlist retains status, logs, repair, and system controls.

use axum::extract::{FromRef, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::auth::AuthState;

#[derive(Clone)]
pub struct DegradedState {
    pub auth: AuthState,
    pub hub: sentryusb_ws::Hub,
    /// Database-open failure for the banner and journal.
    pub reason: std::sync::Arc<String>,
}

impl FromRef<DegradedState> for AuthState {
    fn from_ref(s: &DegradedState) -> Self {
        s.auth.clone()
    }
}

impl FromRef<DegradedState> for sentryusb_ws::Hub {
    fn from_ref(s: &DegradedState) -> Self {
        s.hub.clone()
    }
}

/// GET /api/status — return 200 with a degraded marker so the SPA can
/// distinguish database failure from disconnection.
async fn degraded_status(State(s): State<DegradedState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "degraded": "database_unavailable",
        "degraded_reason": *s.reason,
    }))
}

/// Catch-all for every API route outside the recovery allowlist.
async fn unavailable(State(s): State<DegradedState>) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": "database_unavailable",
            "reason": *s.reason,
        })),
    )
}

pub fn build_degraded_router(state: DegradedState) -> Router {
    Router::new()
        .route("/api/health", get(degraded_status))
        .route("/api/status", get(degraded_status))
        // Initial routing needs setup state before rendering the banner.
        .route("/api/setup/status", get(crate::setup::get_setup_status))
        // Keep the recovery allowlist authenticated.
        .route("/api/auth/login", post(crate::auth::handle_login))
        .route("/api/auth/logout", post(crate::auth::handle_logout))
        .route("/api/auth/check", get(crate::auth::handle_auth_check))
        .route("/api/system/version", get(crate::update::get_version))
        .route("/api/system/reboot", post(crate::system::reboot))
        .route("/api/system/shutdown", post(crate::system::shutdown))
        // USB recording and diagnostics must not require the history database.
        .route("/api/system/gadget-enable", post(crate::system::gadget_enable))
        .route("/api/system/gadget-disable", post(crate::system::gadget_disable))
        .route("/api/diagnostics", get(crate::healthcheck::get_diagnostics))
        .route("/api/diagnostics/refresh", post(crate::healthcheck::refresh_diagnostics))
        .route("/api/diagnostics/download", post(crate::healthcheck::download_diagnostics))
        .route("/api/logs/{name}", get(crate::logs::get_log_tail))
        .route("/api/logs/{name}/page", get(crate::logs::get_log_page))
        .route("/api/logs/{name}/tail", get(crate::logs::get_log_delta))
        .route("/api/storage/health", get(crate::storage_repair::storage_health))
        .route("/api/storage/repair", post(crate::storage_repair::storage_repair))
        .route("/api/ws", get(crate::router::ws_handler))
        .route("/api/{*rest}", axum::routing::any(unavailable))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    fn app() -> Router {
        build_degraded_router(DegradedState {
            auth: AuthState::disabled(),
            hub: sentryusb_ws::Hub::new(),
            reason: std::sync::Arc::new("load: migrate: recording mount unavailable".into()),
        })
    }

    #[tokio::test]
    async fn diagnostics_download_works_without_a_database() {
        // The collector only reads probes/logs; unlike refresh it writes no
        // shared cache file. This exercises the actual degraded route.
        let response = app().oneshot(Request::post("/api/diagnostics/download")
            .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(response.headers()["content-disposition"].to_str().unwrap().contains("sentryusb-diagnostics-"));
        let body = to_bytes(response.into_body(), 4 * 1024 * 1024).await.unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("USB state and recording activity"));
        assert!(text.contains("recording mount startup"));
        assert!(text.contains("load: migrate: recording mount unavailable"));
        assert!(text.contains("database_unavailable"));
    }

    #[tokio::test]
    async fn recovery_routes_are_registered_but_database_routes_still_fail() {
        let response = app().oneshot(Request::get("/api/diagnostics")
            .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // Wrong method proves the POST route exists without manipulating USB
        // hardware or writing the shared diagnostics cache in this test.
        for path in ["/api/diagnostics/refresh", "/api/system/gadget-enable", "/api/system/gadget-disable"] {
            let response = app().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
        }
        let response = app().oneshot(Request::get("/api/drives").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
