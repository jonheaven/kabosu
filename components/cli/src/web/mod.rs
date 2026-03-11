use axum::{
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use config::DogecoinConfig;
use deadpool_postgres;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

mod handlers;

use handlers::*;

/// Capacity of the SSE broadcast channel (events buffered per subscriber lag).
const SSE_CHANNEL_CAPACITY: usize = 256;

/// Shared application state for the web server
#[derive(Clone)]
pub struct AppState {
    pub doginals_pool: Arc<deadpool_postgres::Pool>,
    pub drc20_pool: Option<Arc<deadpool_postgres::Pool>>,
    pub dunes_pool: Option<Arc<deadpool_postgres::Pool>>,
    pub dogecoin_config: DogecoinConfig,
    /// Broadcast channel sender — indexer events arrive via POST /api/webhook
    /// and are fanned out to all /api/events SSE subscribers.
    pub event_tx: broadcast::Sender<String>,
}

/// Start the kabosu web explorer server.
/// Returns the `broadcast::Sender` so the caller can inject the local webhook URL.
pub async fn start_web_server(
    addr: SocketAddr,
    doginals_pool: Arc<deadpool_postgres::Pool>,
    drc20_pool: Option<Arc<deadpool_postgres::Pool>>,
    dunes_pool: Option<Arc<deadpool_postgres::Pool>>,
    _burn_address: String,
    dogecoin_config: DogecoinConfig,
) -> Result<broadcast::Sender<String>, Box<dyn std::error::Error>> {
    let (event_tx, _) = broadcast::channel(SSE_CHANNEL_CAPACITY);
    let state = AppState {
        doginals_pool,
        drc20_pool,
        dunes_pool,
        dogecoin_config,
        event_tx: event_tx.clone(),
    };

    let app = Router::new()
        // API endpoints
        .route("/api/inscriptions", get(get_inscriptions))
        .route("/api/inscriptions/recent", get(get_recent_inscriptions))
        .route("/api/drc20/tokens", get(get_drc20_tokens))
        .route("/api/dunes/tokens", get(get_dunes_tokens))
        .route("/api/lotto/tickets", get(get_lotto_tickets))
        .route("/api/lotto/winners", get(get_lotto_winners))
        .route("/api/lotto/verify", get(lotto_verify))
        .route("/api/dns/names", get(get_dns_names))
        .route("/api/dogemap/claims", get(get_dogemap_claims))
        .route("/api/dogetags", get(get_dogetags))
        .route(
            "/dogespells/balance/:ticker/:address",
            get(get_dogespells_balance),
        )
        .route(
            "/dogespells/history/:ticker/:address",
            get(get_dogespells_history),
        )
        .route("/dogespells/spells/:txid", get(get_dogespells_spells))
        .route("/api/status", get(get_status))
        .route("/api/decode", get(decode_inscription))
        .route("/content/:inscription_id", get(get_inscription_content))
        // HTML pages
        .route("/", get(index_page))
        .route("/inscriptions", get(inscriptions_page))
        .route("/drc20", get(drc20_page))
        .route("/dunes", get(dunes_page))
        .route("/lotto", get(lotto_page))
        // Static assets
        .route("/wallet.js", get(wallet_js))
        // SSE event stream + webhook receiver
        .route("/api/events", get(sse_events))
        .route("/api/webhook", post(receive_webhook))
        // OpenAPI spec
        .route("/openapi.json", get(openapi_spec))
        // Health check
        .route("/health", get(health_check))
        // Marketplace API scaffolding
        .route(
            "/v1/auth/challenge",
            post(create_marketplace_auth_challenge),
        )
        .route("/v1/auth/verify", post(verify_marketplace_auth_challenge))
        .route("/v1/system/health", get(marketplace_health))
        .route("/v1/system/sync", get(marketplace_sync))
        .route(
            "/v1/listings",
            get(list_marketplace_listings).post(create_marketplace_listing),
        )
        .route("/v1/listings/:listing_id", get(get_marketplace_listing))
        .route(
            "/v1/listings/:listing_id/cancel",
            post(cancel_marketplace_listing),
        )
        .route(
            "/v1/orders/:listing_id/build",
            post(build_marketplace_order),
        )
        .route(
            "/v1/orders/:listing_id/submit",
            post(submit_marketplace_order),
        )
        .route("/v1/tx/:txid/status", get(get_marketplace_tx_status))
        .route(
            "/v1/offers",
            get(list_marketplace_offers).post(create_marketplace_offer),
        )
        .route(
            "/v1/offers/:offer_id/cancel",
            post(cancel_marketplace_offer),
        )
        .route(
            "/v1/auctions",
            get(list_marketplace_auctions).post(create_marketplace_auction),
        )
        .route("/v1/auctions/:auction_id", get(get_marketplace_auction))
        .route(
            "/v1/auctions/:auction_id/bids",
            post(create_marketplace_auction_bid),
        )
        .route(
            "/v1/auctions/:auction_id/bids/:bid_id/cancel",
            post(cancel_marketplace_auction_bid),
        )
        .route(
            "/v1/auctions/:auction_id/settle",
            post(settle_marketplace_auction),
        )
        .route(
            "/v1/traders/:address",
            get(get_marketplace_trader).patch(update_marketplace_trader),
        )
        .route(
            "/v1/traders/:address/x/verify",
            post(verify_marketplace_trader_x),
        )
        .route(
            "/v1/traders/:address/activity",
            get(get_marketplace_trader_activity),
        )
        .layer(CorsLayer::permissive())
        .with_state(state);

    println!("🌐 Kabosu explorer starting on http://{}", addr);
    println!("   Visit http://{}/ to view the inscription explorer", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(event_tx)
}

async fn health_check() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "kabosu-explorer"
    }))
}

async fn openapi_spec() -> impl IntoResponse {
    Json(json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Kabosu Explorer API",
            "description": "REST API for the Kabosu Doginals/Dunes indexer. All list endpoints support cursor-based pagination via `offset` and `limit` query parameters.",
            "version": "1.0.0",
            "contact": {
                "url": "https://github.com/yourorg/kabosu"
            }
        },
        "servers": [{ "url": "http://localhost:8080", "description": "Local" }],
        "paths": {
            "/health": {
                "get": {
                    "summary": "Health check",
                    "operationId": "healthCheck",
                    "tags": ["System"],
                    "responses": {
                        "200": {
                            "description": "Service is healthy",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/HealthResponse" } } }
                        }
                    }
                }
            },
            "/api/status": {
                "get": {
                    "summary": "Indexer sync status",
                    "operationId": "getStatus",
                    "tags": ["System"],
                    "responses": {
                        "200": { "description": "Current chain tip and sync progress" }
                    }
                }
            },
            "/api/inscriptions": {
                "get": {
                    "summary": "List inscriptions",
                    "operationId": "getInscriptions",
                    "tags": ["Doginals"],
                    "parameters": [
                        { "name": "offset", "in": "query", "schema": { "type": "integer", "default": 0 } },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 20, "maximum": 100 } },
                        { "name": "mime_type", "in": "query", "schema": { "type": "string" }, "description": "Filter by MIME type prefix, e.g. image/png" }
                    ],
                    "responses": { "200": { "description": "Paginated inscription list" } }
                }
            },
            "/api/inscriptions/recent": {
                "get": {
                    "summary": "Most recently indexed inscriptions",
                    "operationId": "getRecentInscriptions",
                    "tags": ["Doginals"],
                    "parameters": [
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 10, "maximum": 50 } }
                    ],
                    "responses": { "200": { "description": "Recent inscription list" } }
                }
            },
            "/content/{inscription_id}": {
                "get": {
                    "summary": "Raw inscription content",
                    "operationId": "getInscriptionContent",
                    "tags": ["Doginals"],
                    "parameters": [
                        { "name": "inscription_id", "in": "path", "required": true, "schema": { "type": "string" }, "description": "<txid>i<index>" }
                    ],
                    "responses": {
                        "200": { "description": "Raw bytes with original Content-Type" },
                        "404": { "description": "Inscription not found" }
                    }
                }
            },
            "/api/decode": {
                "get": {
                    "summary": "Decode a raw transaction for inscription envelopes",
                    "operationId": "decodeInscription",
                    "tags": ["Doginals"],
                    "parameters": [
                        { "name": "txid", "in": "query", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": { "200": { "description": "Decoded inscription envelope data" } }
                }
            },
            "/api/drc20/tokens": {
                "get": {
                    "summary": "List DRC-20 tokens",
                    "operationId": "getDrc20Tokens",
                    "tags": ["DRC-20"],
                    "parameters": [
                        { "name": "offset", "in": "query", "schema": { "type": "integer", "default": 0 } },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 20, "maximum": 100 } },
                        { "name": "tick", "in": "query", "schema": { "type": "string" }, "description": "Filter by ticker symbol" }
                    ],
                    "responses": { "200": { "description": "DRC-20 token list" } }
                }
            },
            "/api/dunes/tokens": {
                "get": {
                    "summary": "List Dune tokens",
                    "operationId": "getDunesTokens",
                    "tags": ["Dunes"],
                    "parameters": [
                        { "name": "offset", "in": "query", "schema": { "type": "integer", "default": 0 } },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 20, "maximum": 100 } }
                    ],
                    "responses": { "200": { "description": "Dune token list" } }
                }
            },
            "/api/dns/names": {
                "get": {
                    "summary": "List registered DNS names",
                    "operationId": "getDnsNames",
                    "tags": ["DNS"],
                    "parameters": [
                        { "name": "offset", "in": "query", "schema": { "type": "integer", "default": 0 } },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 20, "maximum": 100 } },
                        { "name": "name", "in": "query", "schema": { "type": "string" }, "description": "Exact name lookup" }
                    ],
                    "responses": { "200": { "description": "DNS name list" } }
                }
            },
            "/api/dogemap/claims": {
                "get": {
                    "summary": "List Dogemap block claims",
                    "operationId": "getDogemapClaims",
                    "tags": ["Dogemap"],
                    "parameters": [
                        { "name": "offset", "in": "query", "schema": { "type": "integer", "default": 0 } },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 20, "maximum": 100 } }
                    ],
                    "responses": { "200": { "description": "Dogemap claim list" } }
                }
            },
            "/api/dogetags": {
                "get": {
                    "summary": "List Dogetag on-chain graffiti",
                    "operationId": "getDogetags",
                    "tags": ["Dogetag"],
                    "parameters": [
                        { "name": "offset", "in": "query", "schema": { "type": "integer", "default": 0 } },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 20, "maximum": 100 } }
                    ],
                    "responses": { "200": { "description": "Dogetag list" } }
                }
            },
            "/dogespells/balance/{ticker}/{address}": {
                "get": {
                    "summary": "Get a DogeSpells balance for one ticker/address pair",
                    "operationId": "getDogeSpellsBalance",
                    "tags": ["DogeSpells"],
                    "parameters": [
                        { "name": "ticker", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "address", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": { "200": { "description": "DogeSpells balance snapshot" } }
                }
            },
            "/dogespells/history/{ticker}/{address}": {
                "get": {
                    "summary": "List DogeSpells spells affecting one ticker/address pair",
                    "operationId": "getDogeSpellsHistory",
                    "tags": ["DogeSpells"],
                    "parameters": [
                        { "name": "ticker", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "address", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": { "200": { "description": "DogeSpells spell history" } }
                }
            },
            "/dogespells/spells/{txid}": {
                "get": {
                    "summary": "Fetch all DogeSpells spells emitted by one transaction",
                    "operationId": "getDogeSpellsSpells",
                    "tags": ["DogeSpells"],
                    "parameters": [
                        { "name": "txid", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": { "200": { "description": "DogeSpells spell list for the transaction" } }
                }
            },
            "/api/lotto/tickets": {
                "get": {
                    "summary": "List lotto tickets",
                    "operationId": "getLottoTickets",
                    "tags": ["Lotto"],
                    "parameters": [
                        { "name": "lotto_id", "in": "query", "schema": { "type": "string" } },
                        { "name": "offset", "in": "query", "schema": { "type": "integer", "default": 0 } },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 20, "maximum": 100 } }
                    ],
                    "responses": { "200": { "description": "Lotto ticket list" } }
                }
            },
            "/api/lotto/winners": {
                "get": {
                    "summary": "List lotto winners",
                    "operationId": "getLottoWinners",
                    "tags": ["Lotto"],
                    "parameters": [
                        { "name": "lotto_id", "in": "query", "schema": { "type": "string" } },
                        { "name": "offset", "in": "query", "schema": { "type": "integer", "default": 0 } },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "default": 20, "maximum": 100 } }
                    ],
                    "responses": { "200": { "description": "Lotto winner list" } }
                }
            },
            "/api/lotto/verify": {
                "get": {
                    "summary": "Verify a lotto ticket's drawn numbers",
                    "operationId": "lottoVerify",
                    "tags": ["Lotto"],
                    "parameters": [
                        { "name": "ticket_id", "in": "query", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": { "200": { "description": "Ticket verification result" } }
                }
            },
            "/api/events": {
                "get": {
                    "summary": "Server-Sent Events stream of live indexer events",
                    "operationId": "sseEvents",
                    "tags": ["System"],
                    "responses": {
                        "200": {
                            "description": "SSE stream (text/event-stream)",
                            "content": { "text/event-stream": { "schema": { "type": "string" } } }
                        }
                    }
                }
            },
            "/api/webhook": {
                "post": {
                    "summary": "Internal webhook receiver — fans events out to SSE subscribers",
                    "operationId": "receiveWebhook",
                    "tags": ["System"],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object" } } }
                    },
                    "responses": { "200": { "description": "Accepted" } }
                }
            }
        },
        "components": {
            "schemas": {
                "HealthResponse": {
                    "type": "object",
                    "properties": {
                        "status": { "type": "string", "example": "ok" },
                        "service": { "type": "string", "example": "kabosu-explorer" }
                    }
                }
            }
        },
        "tags": [
            { "name": "System" },
            { "name": "Doginals" },
            { "name": "DRC-20" },
            { "name": "Dunes" },
            { "name": "DNS" },
            { "name": "Dogemap" },
            { "name": "Dogetag" },
            { "name": "DogeSpells" },
            { "name": "Lotto" }
        ]
    }))
}
