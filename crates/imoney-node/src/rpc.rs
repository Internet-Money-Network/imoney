use crate::p2p::PeerManager;
use crate::state::SharedLedger;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use imoney_core::{Address, BlockHeader, Hash, Transaction};
use imoney_pow::MoneyPrinterPow;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};

pub struct AppState {
    pub ledger: SharedLedger,
    pub pow: Arc<MoneyPrinterPow>,
    pub p2p: Arc<PeerManager>,
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

#[derive(Deserialize)]
pub struct BroadcastTxRequest {
    pub transaction: Transaction,
}

#[derive(Serialize)]
pub struct BroadcastTxResponse {
    pub success: bool,
    pub tx_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct AddressBalanceResponse {
    pub address: String,
    pub balance_im: f64,
    pub balance_atoms: u64,
}

#[derive(Serialize)]
pub struct UtxoItemResponse {
    pub transaction_id: String,
    pub index: u32,
    pub value_atoms: u64,
    pub value_im: f64,
}

#[derive(Serialize)]
pub struct WalletGenerateResponse {
    pub address: String,
    pub private_key_hex: String,
    pub public_key_hex: String,
}

#[derive(Deserialize)]
pub struct WalletSendRequest {
    pub private_key_hex: String,
    pub recipient_address: String,
    pub amount_im: f64,
    pub fee_atoms: Option<u64>,
}

#[derive(Serialize)]
pub struct WalletSendResponse {
    pub success: bool,
    pub tx_id: Option<String>,
    pub fee_atoms: u64,
    pub error: Option<String>,
}


#[derive(Serialize)]
pub struct TxStatusResponse {
    pub tx_id: String,
    pub status: String, // "pending", "confirmed", "not_found"
    pub block_hash: Option<String>,
    pub daa_score: Option<u64>,
    pub inputs_count: usize,
    pub outputs_count: usize,
    pub total_output_atoms: u64,
    pub total_output_im: f64,
}

#[derive(Serialize)]
pub struct PeersResponse {
    pub total_connected: usize,
    pub peers: Vec<String>,
}

pub fn create_router(ledger: SharedLedger, pow: Arc<MoneyPrinterPow>, p2p: Arc<PeerManager>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let state = Arc::new(AppState { ledger, pow, p2p });

    Router::new()
        .route("/", get(dashboard_handler))
        .route("/api/v1/info", get(info_handler))
        .route("/api/v1/tips", get(tips_handler))
        .route("/api/v1/peers", get(peers_handler))
        .route("/api/v1/mining/template", get(template_handler))
        .route("/api/v1/mining/submit", post(submit_handler))
        .route("/api/v1/tx/broadcast", post(broadcast_handler))
        .route("/api/v1/tx/:txid", get(tx_status_handler))
        .route("/api/v1/address/:addr/balance", get(balance_handler))
        .route("/api/v1/address/:addr/utxos", get(utxos_handler))
        .route("/api/v1/wallet/generate", post(wallet_generate_handler))
        .route("/api/v1/wallet/send", post(wallet_send_handler))
        .route("/api/v1/ws/address/:addr", get(ws_address_handler))
        .route("/wallet", get(wallet_gui_handler))
        .layer(cors)
        .with_state(state)
}

const WALLET_HTML: &str = include_str!("../../../apps/imoney-wallet/index.html");

async fn wallet_gui_handler() -> Html<&'static str> {
    Html(WALLET_HTML)
}





async fn dashboard_handler(State(state): State<Arc<AppState>>) -> Html<String> {
    let info = state.ledger.read().await.get_info();
    let mining_addr_display = info.mining_address.unwrap_or_else(|| "None (Solo PoW)".to_string());
    
    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Internet Money (IMN) Testnet Node</title>
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <style>
        body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; background: #0d1117; color: #c9d1d9; padding: 2rem; margin: 0; }}
        .card {{ background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 1.5rem; max-width: 850px; margin: 0 auto; box-shadow: 0 4px 12px rgba(0,0,0,0.5); }}
        h1 {{ color: #58a6ff; margin-top: 0; font-size: 1.8rem; }}
        .badge {{ background: #238636; color: white; padding: 0.2rem 0.6rem; border-radius: 4px; font-size: 0.85rem; font-weight: bold; margin-left: 0.5rem; }}
        .row {{ display: flex; justify-content: space-between; padding: 0.75rem 0; border-bottom: 1px solid #21262d; }}
        .row:last-child {{ border-bottom: none; }}
        .hash {{ font-family: monospace; color: #79c0ff; word-break: break-all; }}
        .api-box {{ margin-top: 1.5rem; background: #090d13; border: 1px solid #21262d; border-radius: 6px; padding: 1rem; }}
        code {{ font-family: monospace; color: #39d353; }}
    </style>
</head>
<body>
    <div class="card">
        <h1>Internet Money (IMN) Node <span class="badge">TESTNET-1 (PERSISTENT DB)</span></h1>
        <div class="row"><span>BlockDAG Interval:</span><b>{} seconds (0.2 BPS)</b></div>
        <div class="row"><span>Proof of Work:</span><b>Money Printer (Memory-Hard GPU)</b></div>
        <div class="row"><span>Database Engine:</span><b>ACID On-Disk Storage (redb)</b></div>
        <div class="row"><span>Current Blue Score:</span><b>{}</b></div>
        <div class="row"><span>Total Blocks in DAG:</span><b>{}</b></div>
        <div class="row"><span>Block Subsidy:</span><b>{} IM</b></div>
        <div class="row"><span>Difficulty (Bits):</span><b>{}</b></div>
        <div class="row"><span>Active Mining Address:</span><span class="hash">{}</span></div>
        <div class="row"><span>Virtual Selected Parent:</span><span class="hash">{}</span></div>
        
        <div style="margin-top: 1.5rem; text-align: center;">
            <a href="/wallet" style="display: inline-block; background: #238636; color: white; padding: 0.75rem 1.5rem; border-radius: 6px; text-decoration: none; font-weight: bold; font-size: 1rem; box-shadow: 0 2px 8px rgba(35, 134, 54, 0.4);">👛 Launch Integrated Node & Wallet GUI</a>
        </div>
        
        <div class="api-box">
            <b>🔌 Merchant & Wallet API Endpoints:</b><br><br>
            • GUI Wallet Interface: <code>GET /wallet</code><br>
            • Balance Lookup: <code>GET /api/v1/address/:addr/balance</code><br>
            • Unspent Coins (UTXOs): <code>GET /api/v1/address/:addr/utxos</code><br>
            • Mining Work: <code>GET /api/v1/mining/template</code><br>
            • Block Submission: <code>POST /api/v1/mining/submit</code>
        </div>
    </div>
</body>
</html>"#,
        info.target_block_interval_sec,
        info.virtual_blue_score,
        info.total_blocks,
        info.current_block_reward_im,
        info.current_bits,
        mining_addr_display,
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

async fn peers_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let peers = state.p2p.get_connected_peers().await;
    Json(PeersResponse {
        total_connected: peers.len(),
        peers,
    })
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
    match ledger.add_block(payload.header.clone(), &state.pow) {
        Ok(hash) => {
            state.p2p.broadcast_block(payload.header);
            (
                StatusCode::OK,
                Json(SubmitBlockResponse {
                    success: true,
                    block_hash: Some(hash.to_hex()),
                    error: None,
                }),
            )
        }
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


async fn balance_handler(
    State(state): State<Arc<AppState>>,
    Path(addr_str): Path<String>,
) -> Result<Json<AddressBalanceResponse>, StatusCode> {
    let address = Address::decode(&addr_str).map_err(|_| StatusCode::BAD_REQUEST)?;
    let ledger = state.ledger.read().await;
    let (atoms, coins) = ledger.get_balance(&address).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(AddressBalanceResponse {
        address: addr_str,
        balance_im: coins,
        balance_atoms: atoms,
    }))
}

async fn utxos_handler(
    State(state): State<Arc<AppState>>,
    Path(addr_str): Path<String>,
) -> Result<Json<Vec<UtxoItemResponse>>, StatusCode> {
    let address = Address::decode(&addr_str).map_err(|_| StatusCode::BAD_REQUEST)?;
    let ledger = state.ledger.read().await;
    let utxos = ledger.get_utxos(&address).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let response = utxos
        .into_iter()
        .map(|(outpoint, output)| UtxoItemResponse {
            transaction_id: outpoint.transaction_id.to_hex(),
            index: outpoint.index,
            value_atoms: output.value_atoms,
            value_im: (output.value_atoms as f64) / (imoney_core::constants::SOMPI_PER_IM as f64),
        })
        .collect();

    Ok(Json(response))
}

async fn broadcast_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<BroadcastTxRequest>,
) -> (StatusCode, Json<BroadcastTxResponse>) {
    let mut ledger = state.ledger.write().await;
    match ledger.broadcast_transaction(payload.transaction.clone()) {
        Ok(tx_id) => {
            state.p2p.broadcast_transaction(payload.transaction);
            (
                StatusCode::OK,
                Json(BroadcastTxResponse {
                    success: true,
                    tx_id: Some(tx_id.to_hex()),
                    error: None,
                }),
            )
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(BroadcastTxResponse {
                success: false,
                tx_id: None,
                error: Some(e.to_string()),
            }),
        ),
    }
}


async fn tx_status_handler(
    State(state): State<Arc<AppState>>,
    Path(txid_hex): Path<String>,
) -> Result<Json<TxStatusResponse>, StatusCode> {
    let tx_hash = Hash::from_hex(&txid_hex).map_err(|_| StatusCode::BAD_REQUEST)?;
    let ledger = state.ledger.read().await;

    match ledger.get_transaction(&tx_hash) {
        Ok(Some((tx, maybe_block, maybe_daa))) => {
            let total_out_atoms: u64 = tx.outputs.iter().map(|o| o.value_atoms).sum();
            let total_out_im = (total_out_atoms as f64) / (imoney_core::constants::SOMPI_PER_IM as f64);
            let status = if maybe_block.is_some() { "confirmed" } else { "pending" };

            Ok(Json(TxStatusResponse {
                tx_id: txid_hex,
                status: status.to_string(),
                block_hash: maybe_block.map(|b| b.to_hex()),
                daa_score: maybe_daa,
                inputs_count: tx.inputs.len(),
                outputs_count: tx.outputs.len(),
                total_output_atoms: total_out_atoms,
                total_output_im: total_out_im,
            }))
        }
        Ok(None) => Ok(Json(TxStatusResponse {
            tx_id: txid_hex,
            status: "not_found".to_string(),
            block_hash: None,
            daa_score: None,
            inputs_count: 0,
            outputs_count: 0,
            total_output_atoms: 0,
            total_output_im: 0.0,
        })),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn wallet_generate_handler() -> (StatusCode, Json<WalletGenerateResponse>) {
    let mut csprng = rand::rngs::OsRng;
    let signing_key = ed25519_dalek::SigningKey::generate(&mut csprng);
    let verifying_key = signing_key.verifying_key();

    let addr = Address::from_public_key(
        imoney_core::Network::Testnet,
        imoney_core::AddressType::PubKeyHash,
        verifying_key.as_bytes(),
    );

    let address_str = addr.to_string();
    let private_key_hex = hex::encode(signing_key.to_bytes());
    let public_key_hex = hex::encode(verifying_key.as_bytes());

    (
        StatusCode::OK,
        Json(WalletGenerateResponse {
            address: address_str,
            private_key_hex,
            public_key_hex,
        }),
    )
}

async fn wallet_send_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<WalletSendRequest>,
) -> (StatusCode, Json<WalletSendResponse>) {
    let priv_bytes = match hex::decode(&payload.private_key_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(WalletSendResponse {
                    success: false,
                    tx_id: None,
                    fee_atoms: 0,
                    error: Some("Invalid 32-byte hex private key".to_string()),
                }),
            );
        }
    };

    let signing_key = ed25519_dalek::SigningKey::from_bytes(&priv_bytes);
    let verifying_key = signing_key.verifying_key();
    let sender_addr = Address::from_public_key(
        imoney_core::Network::Testnet,
        imoney_core::AddressType::PubKeyHash,
        verifying_key.as_bytes(),
    );

    let recipient_addr = match Address::decode(&payload.recipient_address) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(WalletSendResponse {
                    success: false,
                    tx_id: None,
                    fee_atoms: 0,
                    error: Some(format!("Invalid recipient address: {}", e)),
                }),
            );
        }
    };

    let fee_atoms = payload.fee_atoms.unwrap_or(10_000); // 0.0001 IM fee
    let amount_atoms = (payload.amount_im * imoney_core::constants::SOMPI_PER_IM as f64).round() as u64;

    let available_utxos = {
        let ledger = state.ledger.read().await;
        match ledger.get_utxos(&sender_addr) {
            Ok(u) => u,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(WalletSendResponse {
                        success: false,
                        tx_id: None,
                        fee_atoms,
                        error: Some(format!("Failed to retrieve UTXOs: {}", e)),
                    }),
                );
            }
        }
    };

    let tx = match Transaction::build_payment(
        &signing_key,
        imoney_core::Network::Testnet,
        &recipient_addr,
        amount_atoms,
        fee_atoms,
        available_utxos,
    ) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(WalletSendResponse {
                    success: false,
                    tx_id: None,
                    fee_atoms,
                    error: Some(e),
                }),
            );
        }
    };

    let mut ledger = state.ledger.write().await;
    match ledger.broadcast_transaction(tx.clone()) {
        Ok(tx_id) => {
            state.p2p.broadcast_transaction(tx);
            (
                StatusCode::OK,
                Json(WalletSendResponse {
                    success: true,
                    tx_id: Some(tx_id.to_hex()),
                    fee_atoms,
                    error: None,
                }),
            )
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(WalletSendResponse {
                success: false,
                tx_id: None,
                fee_atoms,
                error: Some(e.to_string()),
            }),
        ),
    }
}



async fn ws_address_handler(
    ws: WebSocketUpgrade,
    Path(addr_str): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_address_socket(socket, addr_str, state))
}

async fn handle_address_socket(mut socket: WebSocket, addr_str: String, state: Arc<AppState>) {
    let address = match Address::decode(&addr_str) {
        Ok(a) => a,
        Err(_) => {
            let _ = socket
                .send(WsMessage::Text(r#"{"error":"Invalid address"}"#.to_string()))
                .await;
            return;
        }
    };

    let mut last_balance: u64 = {
        let ledger = state.ledger.read().await;
        ledger.get_balance(&address).map(|(atoms, _)| atoms).unwrap_or(0)
    };

    // Send initial balance
    let init_msg = serde_json::json!({
        "event": "connected",
        "address": addr_str,
        "balance_atoms": last_balance,
        "balance_im": (last_balance as f64) / (imoney_core::constants::SOMPI_PER_IM as f64)
    });
    let _ = socket.send(WsMessage::Text(init_msg.to_string())).await;

    // Polling loop every 1 second pushing updates when balance changes
    let mut ticker = tokio::time::interval(Duration::from_millis(1000));
    loop {
        ticker.tick().await;

        let current_balance = {
            let ledger = state.ledger.read().await;
            ledger.get_balance(&address).map(|(atoms, _)| atoms).unwrap_or(0)
        };

        if current_balance != last_balance {
            let change_atoms = current_balance as i64 - last_balance as i64;
            last_balance = current_balance;

            let payment_msg = serde_json::json!({
                "event": "payment_received",
                "address": addr_str,
                "change_atoms": change_atoms,
                "balance_atoms": current_balance,
                "balance_im": (current_balance as f64) / (imoney_core::constants::SOMPI_PER_IM as f64),
                "timestamp_ms": chrono::Utc::now().timestamp_millis()
            });

            if socket.send(WsMessage::Text(payment_msg.to_string())).await.is_err() {
                break;
            }
        }
    }
}

