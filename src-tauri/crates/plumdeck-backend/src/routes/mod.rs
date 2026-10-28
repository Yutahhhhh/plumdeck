//! HTTP routes, one module per Python router.

pub mod dashboard;
pub mod settings;

use axum::routing::get;
use axum::Router;

use crate::AppState;

/// Every ported `/api` route.
pub fn api() -> Router<AppState> {
    Router::new()
        .route("/api/settings", get(settings::get_settings).post(settings::update_setting))
        .route("/api/dashboard", get(dashboard::get_dashboard))
}
