use axum::{
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use deadpool_postgres;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

mod handlers;

use handlers::*;

/// Shared application state for the web server
#[derive(Clone)]
pub struct AppState {
    pub doginals_pool: Arc<deadpool_postgres::Pool>,
    pub drc20_pool: Option<Arc<deadpool_postgres::Pool>>,
    pub dunes_pool: Option<Arc<deadpool_postgres::Pool>>,
}

/// Start the doghook web explorer server
pub async fn start_web_server(
    addr: SocketAddr,
    doginals_pool: Arc<deadpool_postgres::Pool>,
    drc20_pool: Option<Arc<deadpool_postgres::Pool>>,
    dunes_pool: Option<Arc<deadpool_postgres::Pool>>,
    _burn_address: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState {
        doginals_pool,
        drc20_pool,
        dunes_pool,
    };

    let app = Router::new()
        // API endpoints
        .route("/api/inscriptions", get(get_inscriptions))
        .route("/api/inscriptions/recent", get(get_recent_inscriptions))
        .route("/api/drc20/tokens", get(get_drc20_tokens))
        .route("/api/dunes/tokens", get(get_dunes_tokens))
        .route("/api/lotto/tickets", get(get_lotto_tickets))
        .route("/api/lotto/winners", get(get_lotto_winners))
        .route("/api/dns/names", get(get_dns_names))
        .route("/api/dogemap/claims", get(get_dogemap_claims))
        .route("/api/dogetags", get(get_dogetags))
        .route("/api/status", get(get_status))
        // HTML pages
        .route("/", get(index_page))
        .route("/inscriptions", get(inscriptions_page))
        .route("/drc20", get(drc20_page))
        .route("/dunes", get(dunes_page))
        .route("/lotto", get(lotto_page))
        // Health check
        .route("/health", get(health_check))
        .layer(CorsLayer::permissive())
        .with_state(state);

    println!("🌐 Doghook explorer starting on http://{}", addr);
    println!("   Visit http://{}/ to view the inscription explorer", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "doghook-explorer"
    }))
}
