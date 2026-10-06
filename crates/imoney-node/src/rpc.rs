use crate::state::SharedLedger;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use imoney_core::BlockHeader;
use imoney_pow::MoneyPrinterPow;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

pub struct AppState {
    pub ledger: SharedLedger,
    pub pow: Arc<MoneyPrinterPow>,
}

#[derive(Deserialize)]
pub struct SubmitBlockRequest {
    pub header: BlockHeader,
}

#[derive(Serialize)]
pub struct SubmitBlockResponse {
    pub success: bool,
    pub block_hash: Option<String>,
    pub error: Option<String>,
}

pub fn create_router(ledger: SharedLedger, pow: Arc<MoneyPrinterPow>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let state = Arc::new(AppState { ledger, pow });

    Router::new()
        .route("/", get(dashboard_handler))
        .route("/api/v1/info", get(info_handler))
        .route("/api/v1/tips", get(tips_handler))
        .route("/api/v1/mining/template", get(template_handler))
        .route("/api/v1/mining/submit", post(submit_handler))
        .layer(cors)
        .with_state(state)
}

async fn dashboard_handler(State(state): State<Arc<AppState>>) -> Html<String> {
    let info = state.ledger.read().await.get_info();
    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Internet Money (IMN) Testnet Dashboard</title>
    <style>
        body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; background: #0d1117; color: #c9d1d9; padding: 2rem; }}
        .card {{ background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 1.5rem; max-width: 800px; margin: 0 auto; }}
        h1 {{ color: #58a6ff; margin-top: 0; }}
        .badge {{ background: #238636; color: white; padding: 0.2rem 0.6rem; border-radius: 4px; font-size: 0.85rem; font-weight: bold; }}
        .row {{ display: flex; justify-content: space-between; padding: 0.75rem 0; border-bottom: 1px solid #21262d; }}
        .row:last-child {{ border-bottom: none; }}
        .hash {{ font-family: monospace; color: #79c0ff; word-break: break-all; }}
    </style>
</head>
<body>
    <div class="card">
        <h1>Internet Money (IMN) Node <span class="badge">TESTNET-1</span></h1>
        <div class="row"><span>BlockDAG Finality Interval:</span><b>{} seconds (0.2 BPS)</b></div>
        <div class="row"><span>Proof of Work Algorithm:</span><b>Money Printer (Memory-Hard)</b></div>
        <div class="row"><span>Current Blue Score:</span><b>{}</b></div>
        <div class="row"><span>Total Blocks in DAG:</span><b>{}</b></div>
        <div class="row"><span>Current Block Reward:</span><b>{} IM</b></div>
        <div class="row"><span>Difficulty Target (Bits):</span><b>{}</b></div>
        <div class="row"><span>Virtual Selected Parent:</span><span class="hash">{}</span></div>
    </div>
</body>
</html>"#,
        info.target_block_interval_sec,
        info.virtual_blue_score,
        info.total_blocks,
        info.current_block_reward_im,
        info.current_bits,
        info.virtual_selected_parent
    );
    Html(html)
}

async fn info_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let info = state.ledger.read().await.get_info();
    Json(info)
}

async fn tips_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let ledger = state.ledger.read().await;
    let tips: Vec<String> = ledger.tips.iter().map(|t| t.to_hex()).collect();
    Json(tips)
}

async fn template_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let template = state.ledger.read().await.get_mining_template();
    Json(template)
}

async fn submit_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SubmitBlockRequest>,
) -> (StatusCode, Json<SubmitBlockResponse>) {
    let mut ledger = state.ledger.write().await;
    match ledger.add_block(payload.header, &state.pow) {
        Ok(hash) => (
            StatusCode::OK,
            Json(SubmitBlockResponse {
                success: true,
                block_hash: Some(hash.to_hex()),
                error: None,
            }),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(SubmitBlockResponse {
                success: false,
                block_hash: None,
                error: Some(e.to_string()),
            }),
        ),
    }
}
