//! Stratum bridge for Internet Money.
//!
//! Mining software connects here over a plain socket; the bridge fetches work from a node's
//! HTTP API, hands it out, checks what comes back with the light cache, and submits full blocks
//! to the node. Each miner mines to the address it logs in with: the bridge keeps no balances
//! and pays nobody. A pool adds its own accounting on top of the shares it sees.
//!
//! The protocol is described in docs/STRATUM.md.

use clap::Parser;
use imoney_core::{Address, Block, Hash};
use imoney_pow::{compact_to_u256, HallmarkPow, PowMode, PowParams};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Jobs kept per payout address; a share for an older job is rejected as stale.
const JOBS_KEPT: usize = 8;
/// A miner's line may not be longer than this.
const MAX_LINE_BYTES: usize = 4096;
/// A template is refreshed at least this often even when the tips have not changed, so block
/// timestamps stay current.
const TEMPLATE_MAX_AGE: Duration = Duration::from_secs(10);
/// How often share difficulty is reconsidered for each miner.
const RETARGET_EVERY: Duration = Duration::from_secs(30);
/// ...or as soon as a miner has sent this many shares since the last time.
const RETARGET_AFTER_SHARES: u32 = 24;
/// Share targets are the block target made easier by a power of two, up to this many bits.
const MAX_SHIFT: u32 = 24;

const ERR_OTHER: i64 = 20;
const ERR_STALE: i64 = 21;
const ERR_DUPLICATE: i64 = 22;
const ERR_LOW_DIFFICULTY: i64 = 23;
const ERR_UNAUTHORIZED: i64 = 24;

#[derive(Parser, Debug)]
#[command(author, version, about = "Internet Money (IMN) - Stratum bridge for mining software", long_about = None)]
struct Args {
    /// The node's RPC address
    #[arg(short, long, default_value = "http://127.0.0.1:18556")]
    node: String,

    /// Address and port miners connect to
    #[arg(short, long, default_value = "0.0.0.0:18557")]
    bind: String,

    /// Bearer token, if the node was started with --rpc-token
    #[arg(long)]
    rpc_token: Option<String>,

    /// Seconds between shares each miner is steered towards
    #[arg(long, default_value_t = 5)]
    share_interval: u64,

    /// Milliseconds between checks for new work on the node
    #[arg(long, default_value_t = 500)]
    poll_ms: u64,

    /// Most miners connected at once
    #[arg(long, default_value_t = 256)]
    max_miners: usize,
}

#[derive(Deserialize)]
struct Template {
    block: Block,
}

/// Work handed to miners: a block that only lacks its nonce.
struct Job {
    id: String,
    block: Block,
    pre_pow_hash: Hash,
    /// The target a hash must meet to be a block (big-endian).
    target: [u8; 32],
    /// Nonces already submitted for this job.
    seen: Mutex<HashSet<u64>>,
}

/// One connected miner.
struct Miner {
    id: u64,
    /// The top two bytes of every nonce this miner may use, so miners never repeat each other.
    extranonce: u16,
    writer: Mutex<TcpStream>,
    state: Mutex<MinerState>,
}

struct MinerState {
    /// The payout address the miner logged in with.
    address: Option<String>,
    /// Bits by which this miner's share target is easier than the block target.
    shift: u32,
    shares_since_retarget: u32,
    last_retarget: Instant,
    accepted: u64,
    /// Sum of the expected hashes behind each accepted share, for the hashrate estimate.
    work: f64,
    connected: Instant,
}

impl Miner {
    fn send(&self, message: &Value) -> bool {
        let mut line = message.to_string();
        line.push('\n');
        self.writer.lock().unwrap().write_all(line.as_bytes()).is_ok()
    }
}

struct Bridge {
    args: Args,
    pow: HallmarkPow,
    /// What a miner needs to build the dataset: sent in the reply to `mining.subscribe`.
    network: Value,
    miners: Mutex<HashMap<u64, Arc<Miner>>>,
    /// Recent jobs per payout address, newest last.
    jobs: Mutex<HashMap<String, VecDeque<Arc<Job>>>>,
    next_job: AtomicU64,
    blocks_found: AtomicU64,
}

/// `target` made `shift` bits easier, stopping at the largest possible target.
fn shifted_target(target: &[u8; 32], shift: u32) -> [u8; 32] {
    let leading_zero_bits = target
        .iter()
        .position(|byte| *byte != 0)
        .map_or(256, |i| i as u32 * 8 + target[i].leading_zeros());
    if shift >= leading_zero_bits {
        return [0xff; 32];
    }
    let (bytes, bits) = ((shift / 8) as usize, shift % 8);
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let from = i + bytes;
        let high = target.get(from).copied().unwrap_or(0);
        let low = target.get(from + 1).copied().unwrap_or(0);
        *slot = if bits == 0 { high } else { (high << bits) | (low >> (8 - bits)) };
    }
    out
}

/// The shift after seeing `ratio` times as many shares as wanted: each doubling of the share
/// rate makes shares one bit harder, by at most eight bits at a time.
fn retargeted_shift(shift: u32, ratio: f64) -> u32 {
    let steps = ratio.max(f64::MIN_POSITIVE).log2().round().clamp(-8.0, 8.0) as i64;
    (shift as i64 - steps).clamp(0, MAX_SHIFT as i64) as u32
}

/// Expected number of hashes to meet `target`.
fn expected_hashes(target: &[u8; 32]) -> f64 {
    let value = target.iter().fold(0f64, |acc, byte| acc * 256.0 + *byte as f64);
    2f64.powi(256) / (value + 1.0)
}

/// A nonce as miners send it: 16 hexadecimal digits, most significant first.
fn parse_nonce(text: &str) -> Option<u64> {
    let text = text.strip_prefix("0x").unwrap_or(text);
    (text.len() == 16).then(|| u64::from_str_radix(text, 16).ok()).flatten()
}

impl Bridge {
    fn share_target(&self, job: &Job, shift: u32) -> [u8; 32] {
        shifted_target(&job.target, shift)
    }

    fn latest_job(&self, address: &str) -> Option<Arc<Job>> {
        self.jobs.lock().unwrap().get(address).and_then(|jobs| jobs.back().cloned())
    }

    fn find_job(&self, address: &str, id: &str) -> Option<Arc<Job>> {
        self.jobs.lock().unwrap().get(address)?.iter().find(|job| job.id == id).cloned()
    }

    /// Fetches a template for `address` and, if it is new work, stores it and returns it.
    fn refresh(&self, address: &str, max_age: &mut HashMap<String, Instant>) -> Option<Arc<Job>> {
        let url = format!("{}/api/v1/mining/template?address={}", self.args.node, address);
        let template: Template = match ureq::get(&url).call().map_err(|e| e.to_string()).and_then(|r| r.into_json().map_err(|e| e.to_string())) {
            Ok(template) => template,
            Err(e) => {
                eprintln!("[-] Could not fetch work for {}: {}", address, e);
                return None;
            }
        };
        let block = template.block;
        let current = self.latest_job(address);
        let fetched_at = max_age.get(address).copied();
        let same_work = current.as_ref().is_some_and(|job| {
            job.block.header.parents == block.header.parents
                && job.block.header.hash_merkle_root == block.header.hash_merkle_root
                && job.block.header.bits == block.header.bits
        });
        if same_work && fetched_at.is_some_and(|at| at.elapsed() < TEMPLATE_MAX_AGE) {
            return None;
        }

        let pre_pow_hash = block.header.pre_pow_hash().ok()?;
        let job = Arc::new(Job {
            id: format!("{:x}", self.next_job.fetch_add(1, Ordering::Relaxed)),
            target: compact_to_u256(block.header.bits),
            block,
            pre_pow_hash,
            seen: Mutex::new(HashSet::new()),
        });
        let mut jobs = self.jobs.lock().unwrap();
        let queue = jobs.entry(address.to_string()).or_default();
        queue.push_back(job.clone());
        while queue.len() > JOBS_KEPT {
            queue.pop_front();
        }
        max_age.insert(address.to_string(), Instant::now());
        Some(job)
    }

    /// Tells a miner its share target and the job to work on.
    fn send_job(&self, miner: &Miner, job: &Job, shift: u32) -> bool {
        let target = self.share_target(job, shift);
        miner.send(&json!({ "id": null, "method": "mining.set_target", "params": [hex::encode(target)] }))
            && miner.send(&json!({
                "id": null,
                "method": "mining.notify",
                "params": [job.id, job.pre_pow_hash.to_hex(), true],
            }))
    }

    /// Runs forever: keeps every logged-in address supplied with current work.
    fn poll_node(&self) {
        let mut fetched: HashMap<String, Instant> = HashMap::new();
        let mut last_report = Instant::now();
        loop {
            let miners: Vec<Arc<Miner>> = self.miners.lock().unwrap().values().cloned().collect();
            let addresses: HashSet<String> = miners.iter().filter_map(|m| m.state.lock().unwrap().address.clone()).collect();

            for address in &addresses {
                if let Some(job) = self.refresh(address, &mut fetched) {
                    self.notify_address(address, &job);
                }
            }
            // Forget addresses nobody is mining to
            self.jobs.lock().unwrap().retain(|address, _| addresses.contains(address));
            fetched.retain(|address, _| addresses.contains(address));

            if last_report.elapsed() > Duration::from_secs(60) {
                let hashrate: f64 = miners
                    .iter()
                    .map(|m| {
                        let state = m.state.lock().unwrap();
                        state.work / state.connected.elapsed().as_secs_f64().max(1.0)
                    })
                    .sum();
                println!(
                    "[*] {} miners, about {:.2} MH/s, {} blocks found",
                    miners.len(),
                    hashrate / 1e6,
                    self.blocks_found.load(Ordering::Relaxed)
                );
                last_report = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(self.args.poll_ms));
        }
    }

    /// Sends a job to every miner logged in with `address`.
    fn notify_address(&self, address: &str, job: &Job) {
        let miners: Vec<Arc<Miner>> = self.miners.lock().unwrap().values().cloned().collect();
        for miner in miners {
            let shift = {
                let state = miner.state.lock().unwrap();
                if state.address.as_deref() != Some(address) {
                    continue;
                }
                state.shift
            };
            self.send_job(&miner, job, shift);
        }
    }

    /// Sends a block that met the network target to the node. True if the node took it.
    fn submit_block(&self, job: &Job, nonce: u64) -> bool {
        let mut block = job.block.clone();
        block.header.nonce = nonce;
        let mut request = ureq::post(&format!("{}/api/v1/mining/submit", self.args.node));
        if let Some(token) = &self.args.rpc_token {
            request = request.set("Authorization", &format!("Bearer {}", token));
        }
        match request.send_json(json!({ "block": block })) {
            Ok(_) => {
                self.blocks_found.fetch_add(1, Ordering::Relaxed);
                println!("[+] Block accepted at DAA score {}", block.header.daa_score);
                return true;
            }
            // Another block arrived first; routine on a network with parallel blocks
            Err(ureq::Error::Status(400, response)) => {
                let reason = response.into_json::<Value>().ok().and_then(|v| v["error"].as_str().map(str::to_string));
                println!("[*] Block not accepted: {}", reason.unwrap_or_default());
            }
            Err(ureq::Error::Status(401, _)) => eprintln!("[-] The node requires --rpc-token"),
            Err(e) => eprintln!("[-] Could not submit block: {}", e),
        }
        false
    }

    /// Checks one submitted share. Returns the reply's `result` or an error code and message.
    fn handle_submit(&self, miner: &Miner, params: &[Value]) -> Result<Value, (i64, &'static str)> {
        let (address, shift) = {
            let state = miner.state.lock().unwrap();
            (state.address.clone().ok_or((ERR_UNAUTHORIZED, "log in first"))?, state.shift)
        };
        let job_id = params.get(1).and_then(Value::as_str).ok_or((ERR_OTHER, "missing job id"))?;
        let nonce = params
            .get(2)
            .and_then(Value::as_str)
            .and_then(parse_nonce)
            .ok_or((ERR_OTHER, "nonce must be 16 hexadecimal digits"))?;
        if (nonce >> 48) as u16 != miner.extranonce {
            return Err((ERR_OTHER, "nonce does not start with this connection's extranonce"));
        }
        let job = self.find_job(&address, job_id).ok_or((ERR_STALE, "stale job"))?;
        if !job.seen.lock().unwrap().insert(nonce) {
            return Err((ERR_DUPLICATE, "duplicate share"));
        }

        let hash = self.pow.calculate_hash(&job.pre_pow_hash, nonce);
        let share_target = self.share_target(&job, shift);
        if hash.0 > share_target {
            return Err((ERR_LOW_DIFFICULTY, "hash is above the share target"));
        }
        if hash.0 <= job.target && self.submit_block(&job, nonce) {
            // The tips just changed: hand out work on top of the new block without waiting
            if let Some(next) = self.refresh(&address, &mut HashMap::new()) {
                self.notify_address(&address, &next);
            }
        }

        let mut state = miner.state.lock().unwrap();
        state.accepted += 1;
        state.shares_since_retarget += 1;
        state.work += expected_hashes(&share_target);

        // Steer towards one share per interval. A flood of shares is corrected at once
        // instead of waiting for the timer.
        let elapsed = state.last_retarget.elapsed();
        if elapsed >= RETARGET_EVERY || state.shares_since_retarget >= RETARGET_AFTER_SHARES {
            let wanted = elapsed.as_secs_f64().max(0.05) / self.args.share_interval.max(1) as f64;
            let got = state.shares_since_retarget as f64;
            let before = state.shift;
            state.shift = retargeted_shift(before, got / wanted);
            state.shares_since_retarget = 0;
            state.last_retarget = Instant::now();
            if state.shift != before {
                let shift = state.shift;
                drop(state);
                if let Some(latest) = self.latest_job(&address) {
                    self.send_job(miner, &latest, shift);
                }
            }
        }
        Ok(Value::Bool(true))
    }

    fn handle_request(&self, miner: &Miner, method: &str, params: &[Value]) -> Result<Value, (i64, &'static str)> {
        match method {
            "mining.subscribe" => Ok(json!([
                [["mining.notify", format!("{:x}", miner.id)]],
                format!("{:04x}", miner.extranonce),
                6,
                self.network
            ])),
            "mining.authorize" => {
                // "address" or "address.worker"; the worker name is only a label
                let login = params.first().and_then(Value::as_str).ok_or((ERR_OTHER, "missing address"))?;
                let address = login.split('.').next().unwrap_or(login);
                Address::decode(address).map_err(|_| (ERR_UNAUTHORIZED, "the login must be a payout address"))?;
                miner.state.lock().unwrap().address = Some(address.to_string());
                Ok(Value::Bool(true))
            }
            "mining.submit" => self.handle_submit(miner, params),
            "mining.extranonce.subscribe" => Ok(Value::Bool(true)),
            _ => Err((ERR_OTHER, "unknown method")),
        }
    }

    /// Serves one miner until it disconnects.
    fn serve(&self, miner: Arc<Miner>, stream: TcpStream) {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.by_ref().take(MAX_LINE_BYTES as u64).read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) if !line.ends_with('\n') => break, // Too long, or cut off
                Ok(_) => {}
            }
            let Ok(request) = serde_json::from_str::<Value>(&line) else { break };
            let id = request["id"].clone();
            let method = request["method"].as_str().unwrap_or("");
            let params = request["params"].as_array().cloned().unwrap_or_default();

            let reply = match self.handle_request(&miner, method, &params) {
                Ok(result) => json!({ "id": id, "result": result, "error": null }),
                Err((code, message)) => json!({ "id": id, "result": null, "error": [code, message, null] }),
            };
            if !miner.send(&reply) {
                break;
            }

            // Work follows a successful login straight away, without waiting for the next poll
            if method == "mining.authorize" && reply["error"].is_null() {
                let (address, shift) = {
                    let state = miner.state.lock().unwrap();
                    (state.address.clone().unwrap_or_default(), state.shift)
                };
                let job = self.latest_job(&address).or_else(|| self.refresh(&address, &mut HashMap::new()));
                if let Some(job) = job {
                    self.send_job(&miner, &job, shift);
                }
            }
        }
        self.miners.lock().unwrap().remove(&miner.id);
    }
}

fn main() {
    let mut args = Args::parse();
    args.node = args.node.trim_end_matches('/').to_string();

    // The node's genesis hash seeds the proof of work for its network, and it reports the
    // dataset sizes the network uses
    let info = ureq::get(&format!("{}/api/v1/info", args.node))
        .call()
        .ok()
        .and_then(|response| response.into_json::<Value>().ok());
    let genesis = info.as_ref().and_then(|info| info["genesis_hash"].as_str().and_then(|hex| Hash::from_hex(hex).ok()));
    let (Some(info), Some(genesis)) = (info, genesis) else {
        eprintln!("[-] Could not reach the node at {}", args.node);
        std::process::exit(1);
    };
    let params = match (info["pow_light_cache_items"].as_u64(), info["pow_dataset_items"].as_u64()) {
        (Some(cache), Some(dataset)) => PowParams { light_cache_items: cache as u32, dataset_items: dataset as u32 },
        _ => PowParams::dev(),
    };
    println!("[*] Building the verification cache...");
    let pow = HallmarkPow::new(params, genesis, PowMode::Light);
    pow.context();
    let network = json!({
        "genesis_hash": genesis.to_hex(),
        "light_cache_items": params.light_cache_items,
        "dataset_items": params.dataset_items,
    });

    let listener = match TcpListener::bind(&args.bind) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("[-] Could not listen on {}: {}", args.bind, e);
            std::process::exit(1);
        }
    };
    println!("[+] Stratum bridge for {} listening on {}", args.node, args.bind);

    let bridge = Arc::new(Bridge {
        args,
        pow,
        network,
        miners: Mutex::new(HashMap::new()),
        jobs: Mutex::new(HashMap::new()),
        next_job: AtomicU64::new(1),
        blocks_found: AtomicU64::new(0),
    });
    {
        let bridge = bridge.clone();
        std::thread::spawn(move || bridge.poll_node());
    }

    let mut next_id: u64 = 1;
    for stream in listener.incoming().flatten() {
        let Ok(writer) = stream.try_clone() else { continue };
        let _ = stream.set_nodelay(true);
        // A miner that says nothing for ten minutes is gone
        let _ = stream.set_read_timeout(Some(Duration::from_secs(600)));

        let mut miners = bridge.miners.lock().unwrap();
        if miners.len() >= bridge.args.max_miners {
            continue; // Dropping the stream closes it
        }
        // An extranonce no connected miner is using
        let in_use: HashSet<u16> = miners.values().map(|m| m.extranonce).collect();
        let mut extranonce = next_id as u16;
        while in_use.contains(&extranonce) {
            extranonce = extranonce.wrapping_add(1);
        }
        let miner = Arc::new(Miner {
            id: next_id,
            extranonce,
            writer: Mutex::new(writer),
            state: Mutex::new(MinerState {
                address: None,
                // Start easy; the first retarget corrects it
                shift: 12,
                shares_since_retarget: 0,
                last_retarget: Instant::now(),
                accepted: 0,
                work: 0.0,
                connected: Instant::now(),
            }),
        });
        miners.insert(next_id, miner.clone());
        drop(miners);
        next_id += 1;

        let bridge = bridge.clone();
        std::thread::spawn(move || bridge.serve(miner, stream));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifting_a_target_multiplies_it_by_a_power_of_two() {
        let mut target = [0u8; 32];
        target[4] = 0x12;
        target[5] = 0x34;

        assert_eq!(shifted_target(&target, 0), target);

        let mut by_four = [0u8; 32];
        by_four[3] = 0x01;
        by_four[4] = 0x23;
        by_four[5] = 0x40;
        assert_eq!(shifted_target(&target, 4), by_four);

        let mut by_eight = [0u8; 32];
        by_eight[3] = 0x12;
        by_eight[4] = 0x34;
        assert_eq!(shifted_target(&target, 8), by_eight);
    }

    #[test]
    fn shifting_past_the_top_gives_the_easiest_target() {
        let mut target = [0u8; 32];
        target[0] = 0x01;
        // Seven zero bits above the leading one: seven shifts would overflow
        assert_ne!(shifted_target(&target, 6), [0xff; 32]);
        assert_eq!(shifted_target(&target, 7), [0xff; 32]);
        assert_eq!(shifted_target(&[0u8; 32], 1), [0u8; 32]);
    }

    #[test]
    fn an_easier_target_needs_proportionally_fewer_hashes() {
        let mut target = [0u8; 32];
        target[3] = 0x80;
        let ratio = expected_hashes(&target) / expected_hashes(&shifted_target(&target, 5));
        assert!((ratio - 32.0).abs() < 0.001, "{}", ratio);
    }

    #[test]
    fn share_difficulty_follows_the_share_rate() {
        // On target: unchanged
        assert_eq!(retargeted_shift(10, 1.0), 10);
        // Four times too many shares: two bits harder; four times too few: two bits easier
        assert_eq!(retargeted_shift(10, 4.0), 8);
        assert_eq!(retargeted_shift(10, 0.25), 12);
        // A flood is corrected by at most eight bits at once, and never past the block target
        assert_eq!(retargeted_shift(20, 1e9), 12);
        assert_eq!(retargeted_shift(3, 1e9), 0);
        assert_eq!(retargeted_shift(MAX_SHIFT, 0.0), MAX_SHIFT);
    }

    #[test]
    fn nonces_are_sixteen_hex_digits() {
        assert_eq!(parse_nonce("00a1000000000005"), Some(0x00a1_0000_0000_0005));
        assert_eq!(parse_nonce("0x00a1000000000005"), Some(0x00a1_0000_0000_0005));
        assert_eq!(parse_nonce("a1"), None);
        assert_eq!(parse_nonce("zz00000000000000"), None);
    }
}
