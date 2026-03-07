use std::{net::SocketAddr, sync::Arc};

use askama::Template;
use axum::{
    extract::{Path, Query, State},
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use deadpool_postgres::Pool;
use serde::Serialize;

use doginals_indexer::db::doginals_pg;
use postgres::pg_pool_client;

#[derive(Clone)]
pub struct WebState {
    doginals_pool: Arc<Pool>,
    #[allow(dead_code)]
    drc20_pool: Option<Arc<Pool>>,
    #[allow(dead_code)]
    dunes_pool: Option<Arc<Pool>>,
    burn_address: String,
}

#[derive(Template)]
#[template(source = INDEX_HTML, ext = "html")]
struct IndexTemplate;

const INDEX_HTML: &str = include_str!("web/index.html");

#[derive(Serialize)]
struct ListResponse<T> {
    items: Vec<T>,
    total: i64,
}

#[derive(Serialize)]
struct LottoConfigResponse {
    burn_address: String,
}

#[derive(Serialize)]
struct LottoSummaryApi {
    lotto_id: String,
    inscription_id: String,
    deploy_height: u64,
    deploy_timestamp: u64,
    resolved: bool,
}

#[derive(Serialize)]
struct LottoWinnerApi {
    inscription_id: String,
    ticket_id: String,
    rank: u32,
    payout_koinu: u64,
}

#[derive(Serialize)]
struct LottoStatusApi {
    summary: LottoSummaryApi,
    winners: Vec<LottoWinnerApi>,
}

#[derive(Serialize)]
struct LottoTicketCardApi {
    inscription_id: String,
    lotto_id: String,
    ticket_id: String,
    tx_id: String,
    minted_height: u64,
    minted_timestamp: u64,
    seed_numbers: Vec<u16>,
    tip_percent: u8,
}

#[derive(Serialize)]
struct BurnPointsApi {
    owner_address: String,
    burn_points: u64,
    total_tickets_burned: u64,
}

#[derive(serde::Deserialize)]
struct Pagination {
    limit: Option<usize>,
    offset: Option<usize>,
}

pub async fn start_web_server(
    addr: SocketAddr,
    doginals_pool: Arc<Pool>,
    drc20_pool: Option<Arc<Pool>>,
    dunes_pool: Option<Arc<Pool>>,
    burn_address: String,
) -> Result<(), String> {
    let state = WebState {
        doginals_pool,
        drc20_pool,
        dunes_pool,
        burn_address,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/lotto/config", get(lotto_config))
        .route("/api/lotto/list", get(lotto_list))
        .route("/api/lotto/:lotto_id/status", get(lotto_status))
        .route("/api/lotto/:lotto_id/tickets", get(lotto_tickets))
        .route("/api/lotto/burn-points/:owner_address", get(burn_points))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("web bind error: {e}"))?;

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("web server error: {e}"))
}

async fn index() -> impl IntoResponse {
    let html = IndexTemplate
        .render()
        .unwrap_or_else(|_| "<h1>Template error</h1>".to_string());
    Html(html)
}

async fn lotto_config(State(state): State<WebState>) -> impl IntoResponse {
    Json(LottoConfigResponse {
        burn_address: state.burn_address,
    })
}

async fn lotto_list(
    State(state): State<WebState>,
    Query(pagination): Query<Pagination>,
) -> Result<Json<ListResponse<LottoSummaryApi>>, (axum::http::StatusCode, String)> {
    let limit = pagination.limit.unwrap_or(20);
    let offset = pagination.offset.unwrap_or(0);
    let client = pg_pool_client(&state.doginals_pool)
        .await
        .map_err(internal_err)?;
    let items = doginals_pg::list_lotto_lotteries(limit, offset, &client)
        .await
        .map_err(internal_err)?;
    let total = doginals_pg::count_lotto_lotteries(&client)
        .await
        .map_err(internal_err)?;
    let items = items
        .into_iter()
        .map(|r| LottoSummaryApi {
            lotto_id: r.lotto_id,
            inscription_id: r.inscription_id,
            deploy_height: r.deploy_height,
            deploy_timestamp: r.deploy_timestamp,
            resolved: r.resolved,
        })
        .collect();
    Ok(Json(ListResponse { items, total }))
}

async fn lotto_status(
    State(state): State<WebState>,
    Path(lotto_id): Path<String>,
) -> Result<Json<Option<LottoStatusApi>>, (axum::http::StatusCode, String)> {
    let client = pg_pool_client(&state.doginals_pool)
        .await
        .map_err(internal_err)?;
    let item = doginals_pg::get_lotto_lottery(&lotto_id, &client)
        .await
        .map_err(internal_err)?
        .map(|r| LottoStatusApi {
            summary: LottoSummaryApi {
                lotto_id: r.summary.lotto_id,
                inscription_id: r.summary.inscription_id,
                deploy_height: r.summary.deploy_height,
                deploy_timestamp: r.summary.deploy_timestamp,
                resolved: r.summary.resolved,
            },
            winners: r
                .winners
                .into_iter()
                .map(|w| LottoWinnerApi {
                    inscription_id: w.inscription_id,
                    ticket_id: w.ticket_id,
                    rank: w.rank,
                    payout_koinu: w.payout_koinu,
                })
                .collect(),
        });
    Ok(Json(item))
}

async fn lotto_tickets(
    State(state): State<WebState>,
    Path(lotto_id): Path<String>,
    Query(pagination): Query<Pagination>,
) -> Result<Json<ListResponse<LottoTicketCardApi>>, (axum::http::StatusCode, String)> {
    let limit = pagination.limit.unwrap_or(50);
    let offset = pagination.offset.unwrap_or(0);
    let client = pg_pool_client(&state.doginals_pool)
        .await
        .map_err(internal_err)?;
    let items = doginals_pg::list_lotto_tickets(&lotto_id, limit, offset, &client)
        .await
        .map_err(internal_err)?;
    let total = items.len() as i64;
    let items = items
        .into_iter()
        .map(|r| LottoTicketCardApi {
            inscription_id: r.inscription_id,
            lotto_id: r.lotto_id,
            ticket_id: r.ticket_id,
            tx_id: r.tx_id,
            minted_height: r.minted_height,
            minted_timestamp: r.minted_timestamp,
            seed_numbers: r.seed_numbers,
            tip_percent: r.tip_percent,
        })
        .collect();
    Ok(Json(ListResponse { items, total }))
}

async fn burn_points(
    State(state): State<WebState>,
    Path(owner_address): Path<String>,
) -> Result<Json<Option<BurnPointsApi>>, (axum::http::StatusCode, String)> {
    let client = pg_pool_client(&state.doginals_pool)
        .await
        .map_err(internal_err)?;
    let points = doginals_pg::get_burn_points(&owner_address, &client)
        .await
        .map_err(internal_err)?
        .map(|p| BurnPointsApi {
            owner_address: p.owner_address,
            burn_points: p.burn_points,
            total_tickets_burned: p.total_tickets_burned,
        });
    Ok(Json(points))
}

fn internal_err(msg: String) -> (axum::http::StatusCode, String) {
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg)
}
