use crate::p2p::PeerManager;
use crate::state::{LedgerEvent, MiningTemplate, SharedLedger};
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use imoney_core::{Address, Block, Hash, ScriptPublicKey, Transaction};
use imoney_pow::MoneyPrinterPow;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tower_http::cors::{Any, CorsLayer};

pub struct AppState {
    pub ledger: SharedLedger,
    pub pow: Arc<MoneyPrinterPow>,
    pub p2p: Arc<PeerManager>,
    /// When set, mining submission and wallet calls require `Authorization: Bearer <token>`.
    pub rpc_token: Option<String>,
}


#[derive(Deserialize)]
pub struct TemplateQuery {
    /// Payout address for the coinbase. Defaults to the node's own mining address.
    pub address: Option<String>,
}

#[derive(Deserialize)]
pub struct SubmitBlockRequest {
    pub block: Block,
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
    pub balance_imn: f64,
    pub balance_atoms: u64,
}

#[derive(Serialize)]
pub struct UtxoItemResponse {
    pub transaction_id: String,
    pub index: u32,
    pub value_atoms: u64,
    pub value_imn: f64,
    /// False for a block reward that has not matured yet.
    pub spendable: bool,
}

#[derive(Deserialize)]
pub struct InvoiceQuery {
    /// The address the invoice is payable to.
    pub address: String,
}

#[derive(Serialize)]
pub struct InvoicePaymentResponse {
    pub tx_id: String,
    pub amount_atoms: u64,
    pub confirmations: u64,
    /// "seen" (in the mempool), "included" (in a block) or "final".
    pub level: &'static str,
}

/// What has been paid towards an invoice, summed by how settled each payment is.
/// Each total includes the ones after it: `seen_atoms >= included_atoms >= final_atoms`.
#[derive(Serialize)]
pub struct InvoiceResponse {
    pub invoice_id: String,
    pub address: String,
    pub payments: Vec<InvoicePaymentResponse>,
    pub seen_atoms: u64,
    pub included_atoms: u64,
    pub final_atoms: u64,
    /// Confirmations this node requires before it reports a payment as final.
    pub final_confirmations: u64,
    /// True while the network looks unsettled; nothing is reported as final while it is set.
    pub network_alert: bool,
}

#[derive(Serialize)]
pub struct TxStatusResponse {
    pub tx_id: String,
    pub status: String, // "pending", "confirmed", "not_found"
    pub block_hash: Option<String>,
    pub daa_score: Option<u64>,
    /// 0 while pending; 1 once accepted, then one more per blue block added on top.
    pub confirmations: u64,
    pub inputs_count: usize,
    pub outputs_count: usize,
    pub total_output_atoms: u64,
    pub total_output_imn: f64,
}

#[derive(Deserialize)]
pub struct BlocksQuery {
    pub limit: Option<usize>,
}

/// A block as shown in listings: enough to draw the DAG.
#[derive(Serialize)]
pub struct BlockSummary {
    pub hash: String,
    pub parents: Vec<String>,
    pub selected_parent: Option<String>,
    pub timestamp_ms: u64,
    pub bits: String,
    pub daa_score: u64,
    pub blue_score: u64,
    pub is_tip: bool,
}

#[derive(Serialize)]
pub struct BlockDetailResponse {
    #[serde(flatten)]
    pub summary: BlockSummary,
    /// Blue and red blocks this block merges, in consensus order.
    pub mergeset_blues: Vec<String>,
    pub mergeset_reds: Vec<String>,
    pub transaction_ids: Vec<String>,
}

#[derive(Serialize)]
pub struct PeersResponse {
    pub total_connected: usize,
    pub peers: Vec<String>,
}

pub fn create_router(
    ledger: SharedLedger,
    pow: Arc<MoneyPrinterPow>,
    p2p: Arc<PeerManager>,
    rpc_token: Option<String>,
) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let state = Arc::new(AppState { ledger, pow, p2p, rpc_token });

    // Read-only queries and broadcasting an already-signed transaction: callable from any website,
    // which is what lets a merchant's checkout page talk to the merchant's node.
    let public = Router::new()
        .route("/api/v1/info", get(info_handler))
        .route("/api/v1/tips", get(tips_handler))
        .route("/api/v1/peers", get(peers_handler))
        .route("/api/v1/blocks", get(blocks_handler))
        .route("/api/v1/block/:hash", get(block_handler))
        .route("/api/v1/mining/template", get(template_handler))
        .route("/api/v1/tx/broadcast", post(broadcast_handler))
        .route("/api/v1/tx/:txid", get(tx_status_handler))
        .route("/api/v1/address/:addr/balance", get(balance_handler))
        .route("/api/v1/address/:addr/utxos", get(utxos_handler))
        .route("/api/v1/invoice/:id", get(invoice_handler))
        .route("/api/v1/ws/address/:addr", get(ws_address_handler))
        .layer(cors);

    // Adding blocks gets no cross-origin access, so a website open in the operator's browser
    // cannot reach it, and an optional token. The node never handles private keys.
    let restricted = Router::new()
        .route("/api/v1/mining/submit", post(submit_handler))
        .layer(middleware::from_fn_with_state(state.clone(), require_token));

    Router::new()
        .route("/", get(dashboard_handler))
        .route("/wallet", get(wallet_gui_handler))
        .route("/wallet/pkg/imoney_wasm.js", get(wallet_wasm_js_handler))
        .route("/wallet/pkg/imoney_wasm_bg.wasm", get(wallet_wasm_handler))
        .route("/wallet/sdk.js", get(wallet_sdk_handler))
        .merge(public)
        .merge(restricted)
        .with_state(state)
}

/// Rejects the request unless it carries the configured bearer token. No token configured: allowed.
async fn require_token(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    if let Some(expected) = &state.rpc_token {
        let presented = request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or("");
        // Compare without stopping at the first difference
        let matches = presented.len() == expected.len()
            && presented.bytes().zip(expected.bytes()).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0;
        if !matches {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }
    next.run(request).await
}

fn block_summary(ledger: &crate::state::DagLedger, hash: &Hash, header: &imoney_core::BlockHeader) -> BlockSummary {
    let selected_parent = ledger.dag.get(hash).ghostdag.selected_parent;
    BlockSummary {
        hash: hash.to_hex(),
        parents: header.parents.iter().map(|p| p.to_hex()).collect(),
        selected_parent: (selected_parent != Hash::ZERO).then(|| selected_parent.to_hex()),
        timestamp_ms: header.timestamp_ms,
        bits: format!("0x{:08x}", header.bits),
        daa_score: header.daa_score,
        blue_score: header.blue_score,
        is_tip: ledger.tips.contains(hash),
    }
}

/// Most recent blocks, newest first. `limit` defaults to 50 and is capped at 500.
async fn blocks_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<BlocksQuery>,
) -> Json<Vec<BlockSummary>> {
    let ledger = state.ledger.read().await;
    let limit = query.limit.unwrap_or(50).min(500);
    let blocks = ledger
        .recent_blocks(limit)
        .into_iter()
        .map(|(hash, header)| block_summary(&ledger, &hash, header))
        .collect();
    Json(blocks)
}

async fn block_handler(
    State(state): State<Arc<AppState>>,
    Path(hash_hex): Path<String>,
) -> Result<Json<BlockDetailResponse>, StatusCode> {
    let hash = Hash::from_hex(&hash_hex).map_err(|_| StatusCode::BAD_REQUEST)?;
    let ledger = state.ledger.read().await;
    let header = ledger.blocks.get(&hash).ok_or(StatusCode::NOT_FOUND)?;
    let block = ledger
        .storage
        .get_block(&hash)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let ghostdag = &ledger.dag.get(&hash).ghostdag;

    Ok(Json(BlockDetailResponse {
        summary: block_summary(&ledger, &hash, header),
        mergeset_blues: ghostdag.mergeset_blues.iter().map(|h| h.to_hex()).collect(),
        mergeset_reds: ghostdag.mergeset_reds.iter().map(|h| h.to_hex()).collect(),
        transaction_ids: block.transactions.iter().map(|tx| tx.id().to_hex()).collect(),
    }))
}

const WALLET_HTML: &str = include_str!("../../../apps/imoney-wallet/index.html");

// The wallet signs in the browser: the page loads this WebAssembly build of the core crate.
const WALLET_WASM_JS: &str = include_str!("../../../apps/imoney-wallet/pkg/imoney_wasm.js");
const WALLET_WASM: &[u8] = include_bytes!("../../../apps/imoney-wallet/pkg/imoney_wasm_bg.wasm");
const SDK_JS: &str = include_str!("../../../packages/imoney-sdk/dist/imoney.js");

async fn wallet_gui_handler() -> Html<&'static str> {
    Html(WALLET_HTML)
}

async fn wallet_wasm_js_handler() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "text/javascript")], WALLET_WASM_JS)
}

async fn wallet_wasm_handler() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "application/wasm")], WALLET_WASM)
}

async fn wallet_sdk_handler() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "text/javascript")], SDK_JS)
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
        <div class="row"><span>Block Subsidy:</span><b>{} IMN</b></div>
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
            • Invoice Status: <code>GET /api/v1/invoice/:id?address=:addr</code><br>
            • Unspent Coins (UTXOs): <code>GET /api/v1/address/:addr/utxos</code><br>
            • Recent Blocks: <code>GET /api/v1/blocks?limit=50</code><br>
            • Mining Work: <code>GET /api/v1/mining/template?address=:addr</code><br>
            • Block Submission: <code>POST /api/v1/mining/submit</code>
        </div>
    </div>
</body>
</html>"#,
        info.target_block_interval_sec,
        info.virtual_blue_score,
        info.total_blocks,
        info.current_block_reward_imn,
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

async fn template_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TemplateQuery>,
) -> Result<Json<MiningTemplate>, StatusCode> {
    let payout = match query.address {
        Some(addr_str) => Some(Address::decode(&addr_str).map_err(|_| StatusCode::BAD_REQUEST)?),
        None => None,
    };
    let template = state.ledger.read().await.get_mining_template(payout.as_ref());
    Ok(Json(template))
}

async fn submit_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SubmitBlockRequest>,
) -> (StatusCode, Json<SubmitBlockResponse>) {
    let mut ledger = state.ledger.write().await;
    match ledger.add_block(payload.block.clone(), &state.pow) {
        Ok(hash) => {
            state.p2p.broadcast_block(payload.block);
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
        balance_imn: coins,
        balance_atoms: atoms,
    }))
}

async fn utxos_handler(
    State(state): State<Arc<AppState>>,
    Path(addr_str): Path<String>,
) -> Result<Json<Vec<UtxoItemResponse>>, StatusCode> {
    let address = Address::decode(&addr_str).map_err(|_| StatusCode::BAD_REQUEST)?;
    let ledger = state.ledger.read().await;
    let utxos = ledger.get_utxos_with_status(&address).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let response = utxos
        .into_iter()
        .map(|(outpoint, output, spendable)| UtxoItemResponse {
            transaction_id: outpoint.transaction_id.to_hex(),
            index: outpoint.index,
            value_atoms: output.value_atoms,
            value_imn: (output.value_atoms as f64) / (imoney_core::constants::ATOMS_PER_IMN as f64),
            spendable,
        })
        .collect();

    Ok(Json(response))
}

/// Reports every payment that names this invoice and pays the given address.
async fn invoice_handler(
    State(state): State<Arc<AppState>>,
    Path(invoice_id): Path<String>,
    Query(query): Query<InvoiceQuery>,
) -> Result<Json<InvoiceResponse>, StatusCode> {
    if !imoney_core::transaction::is_valid_invoice_id(&invoice_id) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let address = Address::decode(&query.address).map_err(|_| StatusCode::BAD_REQUEST)?;
    let ledger = state.ledger.read().await;
    let payments = ledger.invoice_payments(&invoice_id, &address).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let network_alert = ledger.network_alert();
    let final_confirmations = ledger.final_confirmations;

    let mut response = InvoiceResponse {
        invoice_id,
        address: query.address,
        payments: Vec::new(),
        seen_atoms: 0,
        included_atoms: 0,
        final_atoms: 0,
        final_confirmations,
        network_alert,
    };
    for payment in payments {
        let level = if payment.confirmations == 0 {
            "seen"
        } else if payment.confirmations >= final_confirmations && !network_alert {
            "final"
        } else {
            "included"
        };
        response.seen_atoms = response.seen_atoms.saturating_add(payment.amount_atoms);
        if level != "seen" {
            response.included_atoms = response.included_atoms.saturating_add(payment.amount_atoms);
        }
        if level == "final" {
            response.final_atoms = response.final_atoms.saturating_add(payment.amount_atoms);
        }
        response.payments.push(InvoicePaymentResponse {
            tx_id: payment.tx_id.to_hex(),
            amount_atoms: payment.amount_atoms,
            confirmations: payment.confirmations,
            level,
        });
    }
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
        Ok(Some(info)) => {
            let total_out_atoms: u64 = info.tx.outputs.iter().map(|o| o.value_atoms).sum();
            let total_out_imn = (total_out_atoms as f64) / (imoney_core::constants::ATOMS_PER_IMN as f64);
            let status = if info.block_hash.is_some() { "confirmed" } else { "pending" };
            let daa_score = info.block_hash.and_then(|b| ledger.blocks.get(&b)).map(|h| h.daa_score);

            Ok(Json(TxStatusResponse {
                tx_id: txid_hex,
                status: status.to_string(),
                block_hash: info.block_hash.map(|b| b.to_hex()),
                daa_score,
                confirmations: info.confirmations,
                inputs_count: info.tx.inputs.len(),
                outputs_count: info.tx.outputs.len(),
                total_output_atoms: total_out_atoms,
                total_output_imn: total_out_imn,
            }))
        }
        Ok(None) => Ok(Json(TxStatusResponse {
            tx_id: txid_hex,
            status: "not_found".to_string(),
            block_hash: None,
            daa_score: None,
            confirmations: 0,
            inputs_count: 0,
            outputs_count: 0,
            total_output_atoms: 0,
            total_output_imn: 0.0,
        })),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
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
    let script = ScriptPublicKey::pay_to_address(&address);
    let atoms_per_imn = imoney_core::constants::ATOMS_PER_IMN as f64;

    // Subscribe before reading the balance so no change can fall between the two
    let (mut events, mut last_balance) = {
        let ledger = state.ledger.read().await;
        (ledger.events.subscribe(), ledger.get_balance(&address).map(|(atoms, _)| atoms).unwrap_or(0))
    };

    // Send initial balance
    let init_msg = serde_json::json!({
        "event": "connected",
        "address": addr_str,
        "balance_atoms": last_balance,
        "balance_imn": (last_balance as f64) / atoms_per_imn
    });
    if socket.send(WsMessage::Text(init_msg.to_string())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            event = events.recv() => {
                let message = match event {
                    // A payment to this address reached the mempool: seen, not yet in a block
                    Ok(LedgerEvent::PendingTx { tx_id, outputs, invoice_id }) => {
                        let amount_atoms: u64 = outputs
                            .iter()
                            .filter(|o| o.script_public_key == script)
                            .map(|o| o.value_atoms)
                            .sum();
                        (amount_atoms > 0).then(|| serde_json::json!({
                            "event": "payment_seen",
                            "address": addr_str,
                            "tx_id": tx_id.to_hex(),
                            "invoice_id": invoice_id,
                            "amount_atoms": amount_atoms,
                            "amount_imn": (amount_atoms as f64) / atoms_per_imn,
                            "timestamp_ms": chrono::Utc::now().timestamp_millis()
                        }))
                    }
                    // The ledger moved (or events were skipped): report a balance change if any
                    Ok(LedgerEvent::BlockAdded { .. }) | Err(RecvError::Lagged(_)) => {
                        let current_balance = {
                            let ledger = state.ledger.read().await;
                            ledger.get_balance(&address).map(|(atoms, _)| atoms).unwrap_or(last_balance)
                        };
                        let change_atoms = current_balance as i64 - last_balance as i64;
                        last_balance = current_balance;
                        (change_atoms != 0).then(|| serde_json::json!({
                            "event": "payment_received",
                            "address": addr_str,
                            "change_atoms": change_atoms,
                            "balance_atoms": current_balance,
                            "balance_imn": (current_balance as f64) / atoms_per_imn,
                            "timestamp_ms": chrono::Utc::now().timestamp_millis()
                        }))
                    }
                    Err(RecvError::Closed) => break,
                };
                if let Some(message) = message {
                    if socket.send(WsMessage::Text(message.to_string())).await.is_err() {
                        break;
                    }
                }
            }
            // Notice when the client goes away instead of holding the subscription forever
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}
