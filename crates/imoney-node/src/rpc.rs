use crate::p2p::PeerManager;
use crate::state::{LedgerEvent, MiningTemplate, NetworkStats, SharedLedger};
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use imoney_core::{Address, Block, Hash, ScriptPublicKey, Transaction};
use imoney_pow::HallmarkPow;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tower_http::cors::{Any, CorsLayer};

pub struct AppState {
    pub ledger: SharedLedger,
    pub pow: Arc<HallmarkPow>,
    pub p2p: Arc<PeerManager>,
    /// When set, mining submission and wallet calls require `Authorization: Bearer <token>`.
    pub rpc_token: Option<String>,
    /// The last computed network statistics and the tip they were computed for.
    pub stats_cache: std::sync::Mutex<Option<(Hash, NetworkStats)>>,
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
    /// 1 once accepted into the ledger, then one more per blue block added on top.
    pub confirmations: u64,
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    /// Rows to return, newest first (default 50, at most 500).
    pub limit: Option<usize>,
    /// The `next` value of the previous page, to continue after it.
    pub before: Option<String>,
}

#[derive(Serialize)]
pub struct HistoryItemResponse {
    /// The transaction. For a mining reward there is no transaction; this is the id of the
    /// coin the reward created, as listed by the coins endpoint.
    pub tx_id: String,
    /// "received", "sent" or "reward". A payment with change counts as "sent".
    pub kind: &'static str,
    pub received_atoms: u64,
    pub sent_atoms: u64,
    /// `received_atoms - sent_atoms`: what the transaction did to the balance.
    pub net_atoms: i128,
    pub net_imn: f64,
    pub confirmations: u64,
    /// Time of the block that carries the transaction, as its miner reported it.
    pub timestamp_ms: u64,
}

#[derive(Serialize)]
pub struct HistoryResponse {
    pub address: String,
    pub items: Vec<HistoryItemResponse>,
    /// Pass as `before` to read the next, older page. Absent on the last page.
    pub next: Option<String>,
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
    /// The address the payment came from. A refund goes here unless the customer names
    /// another address: a payment sent from an exchange comes from the exchange's address.
    pub payer_address: Option<String>,
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
    /// The coins the payment spends, as `transaction_id:index` of the outputs that created them.
    pub inputs: Vec<String>,
    /// Where the money went. An output without an address uses a script type this node cannot name.
    pub outputs: Vec<TxOutputView>,
    /// The addresses whose coins the payment spends, without repeats: who paid.
    pub senders: Vec<String>,
    /// The invoice the payment names, if any.
    pub invoice_id: Option<String>,
    /// The node address that receives half of the fee, if the payment names one.
    pub service_address: Option<String>,
    pub size_bytes: usize,
}

#[derive(Serialize)]
pub struct TxOutputView {
    pub address: Option<String>,
    pub amount_atoms: u64,
    pub amount_imn: f64,
}

/// Network statistics plus the figures that change between blocks.
#[derive(Serialize)]
pub struct StatsResponse {
    #[serde(flatten)]
    pub stats: NetworkStats,
    pub network: String,
    /// The tip the selected chain ends in.
    pub selected_tip: String,
    pub blue_score: u64,
    pub daa_score: u64,
    pub total_blocks: usize,
    pub tips: usize,
    pub mempool_size: usize,
    pub peers: usize,
    pub finality_depth: u64,
    pub network_alert: bool,
    pub archival: bool,
    pub min_relay_fee_atoms_per_byte: u64,
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
    /// The address the block's reward is paid to, when the coinbase names one.
    pub miner_address: Option<String>,
    pub reward_imn: f64,
    pub size_bytes: usize,
    /// Expected number of hashes needed to mine this block.
    pub difficulty: f64,
    pub nonce: u64,
}

#[derive(Serialize)]
pub struct PeersResponse {
    pub total_connected: usize,
    pub peers: Vec<String>,
}

pub fn create_router(
    ledger: SharedLedger,
    pow: Arc<HallmarkPow>,
    p2p: Arc<PeerManager>,
    rpc_token: Option<String>,
) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let state = Arc::new(AppState { ledger, pow, p2p, rpc_token, stats_cache: std::sync::Mutex::new(None) });

    // Read-only queries and broadcasting an already-signed transaction: callable from any website,
    // which is what lets a merchant's checkout page talk to the merchant's node.
    let public = Router::new()
        .route("/api/v1/info", get(info_handler))
        .route("/api/v1/tips", get(tips_handler))
        .route("/api/v1/peers", get(peers_handler))
        .route("/api/v1/stats", get(stats_handler))
        .route("/api/v1/blocks", get(blocks_handler))
        .route("/api/v1/block/:hash", get(block_handler))
        .route("/api/v1/mining/template", get(template_handler))
        .route("/api/v1/tx/broadcast", post(broadcast_handler))
        .route("/api/v1/tx/:txid", get(tx_status_handler))
        .route("/api/v1/address/:addr/balance", get(balance_handler))
        .route("/api/v1/address/:addr/utxos", get(utxos_handler))
        .route("/api/v1/address/:addr/history", get(history_handler))
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
        .route("/explorer", get(explorer_handler))
        .route("/demo", get(demo_handler))
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

/// The readable address of a locking script, when it is a known address type.
fn script_address(network: imoney_core::Network, script: &ScriptPublicKey) -> Option<String> {
    let address_type = imoney_core::AddressType::from_u8(script.version)?;
    let hash: [u8; 32] = script.script.as_slice().try_into().ok()?;
    Some(Address::new(network, address_type, Hash(hash)).to_string())
}

/// Hashrate, difficulty, supply and the other network-wide figures. The expensive part is
/// computed once per new tip and reused until the tip changes.
async fn stats_handler(State(state): State<Arc<AppState>>) -> Result<Json<StatsResponse>, StatusCode> {
    let peers = state.p2p.get_connected_peers().await.len();
    let ledger = state.ledger.read().await;
    let tip = ledger.virtual_selected_parent;

    let cached = state.stats_cache.lock().unwrap().as_ref().filter(|(at, _)| *at == tip).map(|(_, stats)| stats.clone());
    let stats = match cached {
        Some(stats) => stats,
        None => {
            let stats = ledger.network_stats().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            *state.stats_cache.lock().unwrap() = Some((tip, stats.clone()));
            stats
        }
    };
    let info = ledger.get_info();
    Ok(Json(StatsResponse {
        stats,
        network: info.network,
        selected_tip: info.virtual_selected_parent,
        blue_score: info.virtual_blue_score,
        daa_score: info.virtual_daa_score,
        total_blocks: info.total_blocks,
        tips: info.tips.len(),
        mempool_size: info.mempool_size,
        peers,
        finality_depth: info.finality_depth,
        network_alert: info.network_alert,
        archival: info.archival,
        min_relay_fee_atoms_per_byte: crate::mempool::MIN_RELAY_FEE_PER_BYTE,
    }))
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
    let header = &ledger.header(&hash).ok_or(StatusCode::NOT_FOUND)?;
    let block = ledger
        .storage
        .get_block(&hash)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let dag_block = ledger.dag.get(&hash);
    let ghostdag = &dag_block.ghostdag;

    Ok(Json(BlockDetailResponse {
        summary: block_summary(&ledger, &hash, header),
        mergeset_blues: ghostdag.mergeset_blues.iter().map(|h| h.to_hex()).collect(),
        mergeset_reds: ghostdag.mergeset_reds.iter().map(|h| h.to_hex()).collect(),
        transaction_ids: block.transactions.iter().map(|tx| tx.id().to_hex()).collect(),
        miner_address: block
            .transactions
            .first()
            .and_then(|coinbase| coinbase.outputs.first())
            .and_then(|output| script_address(ledger.network, &output.script_public_key)),
        reward_imn: imoney_emission::block_subsidy_imn(header.daa_score),
        size_bytes: imoney_core::Encode::to_bytes(&block).len(),
        difficulty: dag_block.work as f64,
        nonce: header.nonce,
    }))
}

const WALLET_HTML: &str = include_str!("../../../apps/imoney-wallet/index.html");

// The wallet signs in the browser: the page loads this WebAssembly build of the core crate.
const WALLET_WASM_JS: &str = include_str!("../../../apps/imoney-wallet/pkg/imoney_wasm.js");
const WALLET_WASM: &[u8] = include_bytes!("../../../apps/imoney-wallet/pkg/imoney_wasm_bg.wasm");
const SDK_JS: &str = include_str!("../../../packages/imoney-sdk/dist/imoney.js");

const EXPLORER_HTML: &str = include_str!("../../../apps/imoney-explorer/index.html");

async fn wallet_gui_handler() -> Html<&'static str> {
    Html(WALLET_HTML)
}

async fn explorer_handler() -> Html<&'static str> {
    Html(EXPLORER_HTML)
}

/// A sample shop checkout that takes a payment through this node.
async fn demo_handler() -> Html<&'static str> {
    Html(include_str!("../../../examples/e2e-payment-demo/index.html"))
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





const HOME_HTML: &str = include_str!("../../../apps/imoney-home/index.html");

/// The node's front page. It reads the node's status from the API like any other client.
async fn dashboard_handler() -> Html<&'static str> {
    Html(HOME_HTML)
}

async fn info_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let info = state.ledger.read().await.get_info();
    // Miners build their dataset from these sizes
    let mut info = serde_json::to_value(info).unwrap_or_default();
    info["pow_light_cache_items"] = state.pow.params().light_cache_items.into();
    info["pow_dataset_items"] = state.pow.params().dataset_items.into();
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
        .map(|(outpoint, output, spendable, confirmations)| UtxoItemResponse {
            transaction_id: outpoint.transaction_id.to_hex(),
            index: outpoint.index,
            value_atoms: output.value_atoms,
            value_imn: (output.value_atoms as f64) / (imoney_core::constants::ATOMS_PER_IMN as f64),
            spendable,
            confirmations,
        })
        .collect();

    Ok(Json(response))
}

/// What an address received and sent, newest first. Only transactions accepted into the
/// ledger are listed; a pruned node lists only those it still keeps.
async fn history_handler(
    State(state): State<Arc<AppState>>,
    Path(addr_str): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryResponse>, StatusCode> {
    let address = Address::decode(&addr_str).map_err(|_| StatusCode::BAD_REQUEST)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let before = match &query.before {
        Some(position) => Some(hex::decode(position).ok().filter(|p| p.len() == 40).ok_or(StatusCode::BAD_REQUEST)?),
        None => None,
    };
    let ledger = state.ledger.read().await;
    let rows = ledger
        .address_history(&address, limit, before.as_deref())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let next = (rows.len() == limit).then(|| rows.last().map(|(row, _)| hex::encode(row.position()))).flatten();
    let items = rows
        .into_iter()
        .map(|(row, confirmations)| {
            let net_atoms = row.received_atoms as i128 - row.sent_atoms as i128;
            HistoryItemResponse {
                tx_id: row.id.to_hex(),
                kind: if row.is_reward {
                    "reward"
                } else if row.sent_atoms > 0 {
                    "sent"
                } else {
                    "received"
                },
                received_atoms: row.received_atoms,
                sent_atoms: row.sent_atoms,
                net_atoms,
                net_imn: net_atoms as f64 / imoney_core::constants::ATOMS_PER_IMN as f64,
                confirmations,
                timestamp_ms: row.timestamp_ms,
            }
        })
        .collect();
    Ok(Json(HistoryResponse { address: addr_str, items, next }))
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
            payer_address: payment.payer.map(|address| address.to_string()),
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
            let daa_score = info.block_hash.and_then(|b| ledger.header(&b)).map(|h| h.daa_score);

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
                inputs: info
                    .tx
                    .inputs
                    .iter()
                    .map(|input| format!("{}:{}", input.previous_outpoint.transaction_id.to_hex(), input.previous_outpoint.index))
                    .collect(),
                outputs: info
                    .tx
                    .outputs
                    .iter()
                    .map(|output| TxOutputView {
                        address: script_address(ledger.network, &output.script_public_key),
                        amount_atoms: output.value_atoms,
                        amount_imn: (output.value_atoms as f64) / (imoney_core::constants::ATOMS_PER_IMN as f64),
                    })
                    .collect(),
                senders: {
                    let mut senders: Vec<String> = (0..info.tx.inputs.len())
                        .filter_map(|index| info.tx.input_address(ledger.network, index))
                        .map(|address| address.to_string())
                        .collect();
                    senders.dedup();
                    senders
                },
                invoice_id: info.tx.invoice_id().map(str::to_string),
                service_address: info.tx.service.as_ref().and_then(|script| script_address(ledger.network, script)),
                size_bytes: imoney_core::Encode::to_bytes(&info.tx).len(),
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
            inputs: Vec::new(),
            outputs: Vec::new(),
            senders: Vec::new(),
            invoice_id: None,
            service_address: None,
            size_bytes: 0,
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
