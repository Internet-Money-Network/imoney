//! OpenCL GPU miner for Money Printer, Internet Money's proof of work (FishHash).
//!
//! The dataset is built on the CPU with the same code the node uses and copied to the graphics
//! card; the card searches nonces. Every nonce the card reports is checked on the CPU before it
//! is submitted, so a faulty driver or card cannot produce an invalid block.

use clap::Parser;
use imoney_core::{Block, Hash};
use imoney_pow::target::compact_to_u256;
use imoney_pow::{MoneyPrinterPow, PowMode, PowParams};
use opencl3::command_queue::CommandQueue;
use opencl3::context::Context;
use opencl3::device::{Device, CL_DEVICE_TYPE_GPU};
use opencl3::kernel::{ExecuteKernel, Kernel};
use opencl3::memory::{Buffer, CL_MEM_READ_ONLY, CL_MEM_READ_WRITE};
use opencl3::platform::get_platforms;
use opencl3::program::Program;
use opencl3::types::{cl_uint, cl_ulong, CL_BLOCKING};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Error = Box<dyn std::error::Error>;

const KERNEL_SOURCE: &str = include_str!("search.cl");
/// Must match MAX_RESULTS in search.cl
const MAX_RESULTS: usize = 4;
/// How long one block template is searched before a fresh one is fetched
const TEMPLATE_LIFETIME: Duration = Duration::from_millis(1000);

#[derive(Parser, Debug)]
#[command(author, version, about = "Internet Money (IMN) - Money Printer OpenCL GPU miner", long_about = None)]
struct Args {
    /// Mine for a node: its RPC address, e.g. http://127.0.0.1:18556
    #[arg(short, long)]
    node: Option<String>,

    /// Mine through a Stratum bridge or pool instead of a node: its host and port
    #[arg(short, long)]
    stratum: Option<String>,

    /// Payout address for mined blocks (default: the node's own mining address).
    /// Required with --stratum.
    #[arg(short, long)]
    address: Option<String>,

    /// Bearer token, if the node was started with --rpc-token
    #[arg(long)]
    rpc_token: Option<String>,

    /// List the graphics cards OpenCL can see and exit
    #[arg(long, default_value_t = false)]
    list_devices: bool,

    /// Which card to use, by its number in --list-devices
    #[arg(short, long, default_value_t = 0)]
    device: usize,

    /// Nonces per batch, as a power of two
    #[arg(short, long, default_value_t = 22)]
    intensity: u32,

    /// Dataset size for --benchmark: "full" (4.6 GB, the public networks) or "dev" (32 MB,
    /// private test networks). When mining, the size comes from the node or the pool.
    #[arg(long, default_value = "full")]
    pow_size: String,

    /// Check the card's hashes against the CPU implementation, then measure the hashrate
    #[arg(short, long, default_value_t = false)]
    benchmark: bool,

    /// Stop mining after this many seconds (default: run until stopped)
    #[arg(long)]
    duration: Option<u64>,
}

#[derive(Deserialize)]
struct Template {
    block: Block,
}

#[derive(Deserialize)]
struct SubmitResponse {
    success: bool,
    block_hash: Option<String>,
    error: Option<String>,
}

fn gpu_devices() -> Result<Vec<Device>, Error> {
    let mut devices = Vec::new();
    for platform in get_platforms()? {
        for id in platform.get_devices(CL_DEVICE_TYPE_GPU)? {
            devices.push(Device::new(id));
        }
    }
    Ok(devices)
}

/// A graphics card holding the dataset and ready to search nonces.
struct GpuSearcher {
    queue: CommandQueue,
    kernel: Kernel,
    dataset: Buffer<cl_uint>,
    dataset_items: cl_uint,
    header: Buffer<cl_uint>,
    target: Buffer<cl_uint>,
    results: Buffer<cl_uint>,
    // Dropped last: the buffers and queue above belong to it
    _program: Program,
    _context: Context,
}

impl GpuSearcher {
    fn new(device: &Device, dataset_words: &[[u32; 32]]) -> Result<Self, Error> {
        let context = Context::from_device(device)?;
        let queue = CommandQueue::create_default(&context, 0)?;
        let program = Program::create_and_build_from_source(&context, KERNEL_SOURCE, "")
            .map_err(|log| format!("the OpenCL kernel did not compile:\n{}", log))?;
        let kernel = Kernel::create(&program, "search")?;

        let flat = dataset_words.as_flattened();
        // SAFETY: each buffer is created with no host pointer and written from a slice of
        // exactly the length it was created with.
        let (dataset, header, target, results) = unsafe {
            let mut dataset = Buffer::<cl_uint>::create(&context, CL_MEM_READ_ONLY, flat.len(), ptr::null_mut())?;
            queue.enqueue_write_buffer(&mut dataset, CL_BLOCKING, 0, flat, &[])?;
            let header = Buffer::<cl_uint>::create(&context, CL_MEM_READ_ONLY, 8, ptr::null_mut())?;
            let target = Buffer::<cl_uint>::create(&context, CL_MEM_READ_ONLY, 8, ptr::null_mut())?;
            let mut results =
                Buffer::<cl_uint>::create(&context, CL_MEM_READ_WRITE, 1 + 2 * MAX_RESULTS, ptr::null_mut())?;
            queue.enqueue_write_buffer(&mut results, CL_BLOCKING, 0, &[0; 1 + 2 * MAX_RESULTS], &[])?;
            (dataset, header, target, results)
        };

        Ok(Self {
            queue,
            kernel,
            dataset,
            dataset_items: dataset_words.len() as cl_uint,
            header,
            target,
            results,
            _program: program,
            _context: context,
        })
    }

    /// Sets the block being mined and the target its hash must not exceed (big-endian bytes).
    fn set_work(&mut self, pre_pow_hash: &Hash, target: &[u8; 32]) -> Result<(), Error> {
        let mut header_words = [0u32; 8];
        let mut target_words = [0u32; 8];
        for i in 0..8 {
            header_words[i] = u32::from_le_bytes(pre_pow_hash.0[4 * i..4 * i + 4].try_into().unwrap());
            target_words[i] = u32::from_be_bytes(target[4 * i..4 * i + 4].try_into().unwrap());
        }
        // SAFETY: both buffers hold 8 words
        unsafe {
            self.queue.enqueue_write_buffer(&mut self.header, CL_BLOCKING, 0, &header_words, &[])?;
            self.queue.enqueue_write_buffer(&mut self.target, CL_BLOCKING, 0, &target_words, &[])?;
        }
        Ok(())
    }

    /// Tries `count` nonces from `start_nonce` and returns those that met the target.
    fn search(&mut self, start_nonce: u64, count: usize) -> Result<Vec<u64>, Error> {
        let mut found = [0 as cl_uint; 1 + 2 * MAX_RESULTS];
        // SAFETY: the arguments match the kernel's parameters in order and type, and the
        // results buffer is as long as `found`.
        unsafe {
            ExecuteKernel::new(&self.kernel)
                .set_arg(&self.dataset)
                .set_arg(&self.dataset_items)
                .set_arg(&self.header)
                .set_arg(&(start_nonce as cl_ulong))
                .set_arg(&self.target)
                .set_arg(&self.results)
                .set_global_work_size(count)
                .enqueue_nd_range(&self.queue)?;
            self.queue.enqueue_read_buffer(&self.results, CL_BLOCKING, 0, &mut found, &[])?;
        }
        if found[0] == 0 {
            return Ok(Vec::new());
        }
        // SAFETY: as above
        unsafe {
            self.queue.enqueue_write_buffer(&mut self.results, CL_BLOCKING, 0, &[0 as cl_uint], &[])?;
        }
        let reported = (found[0] as usize).min(MAX_RESULTS);
        Ok((0..reported).map(|i| found[1 + 2 * i] as u64 | (found[2 + 2 * i] as u64) << 32).collect())
    }
}

fn main() {
    if let Err(e) = run(Args::parse()) {
        eprintln!("[-] {}", e);
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<(), Error> {
    let devices = gpu_devices().map_err(|e| format!("OpenCL is not available: {}", e))?;
    if args.list_devices {
        for (i, device) in devices.iter().enumerate() {
            println!(
                "{}: {} ({}, {} MB)",
                i,
                device.name().unwrap_or_default().trim(),
                device.vendor().unwrap_or_default().trim(),
                device.global_mem_size().unwrap_or(0) / (1 << 20)
            );
        }
        return Ok(());
    }
    let device = devices.get(args.device).ok_or("no such graphics card; see --list-devices")?;
    println!("[*] Card: {}", device.name().unwrap_or_default().trim());

    let batch = 1usize << args.intensity.min(30);

    if let Some(server) = &args.stratum {
        return mine_stratum(device, server, &args, batch);
    }

    let Some(node) = args.node.as_deref().map(|n| n.trim_end_matches('/')) else {
        if !args.benchmark {
            return Err("give --node or --stratum to mine, or --benchmark".into());
        }
        let params = match args.pow_size.as_str() {
            "full" | "mainnet" => PowParams::mainnet(),
            "dev" => PowParams::dev(),
            other => return Err(format!("unknown --pow-size {}", other).into()),
        };
        let pow = MoneyPrinterPow::new(params, Hash::from_bytes([0x42; 32]), PowMode::Full);
        let mut gpu = load(device, &pow)?;
        return benchmark(&mut gpu, &pow, batch);
    };

    // The node's genesis hash seeds the proof of work for its network
    let info: serde_json::Value = ureq::get(&format!("{}/api/v1/info", node)).call()?.into_json()?;
    let genesis = Hash::from_hex(info["genesis_hash"].as_str().ok_or("node did not report a genesis hash")?)?;
    let pow = MoneyPrinterPow::new(pow_params_of(&info), genesis, PowMode::Full);
    let mut gpu = load(device, &pow)?;
    println!("[+] Connected to {} ({})", node, info["network"].as_str().unwrap_or("unknown network"));
    mine(&mut gpu, &pow, node, &args, batch)
}

/// The dataset sizes a node reports for its network. A node from before it reported them
/// is a small-dataset one.
fn pow_params_of(info: &serde_json::Value) -> PowParams {
    match (info["pow_light_cache_items"].as_u64(), info["pow_dataset_items"].as_u64()) {
        (Some(cache), Some(dataset)) => PowParams { light_cache_items: cache as u32, dataset_items: dataset as u32 },
        _ => PowParams::dev(),
    }
}

/// Builds the dataset on the CPU and copies it to the card.
fn load(device: &Device, pow: &MoneyPrinterPow) -> Result<GpuSearcher, Error> {
    println!("[*] Building the mining dataset on the CPU...");
    let started = Instant::now();
    let context = pow.context();
    let words = context.dataset_words().ok_or("the dataset was not built")?;
    println!("[+] {} items ({} MB) in {:.1?}", words.len(), words.len() * 128 / (1 << 20), started.elapsed());
    let started = Instant::now();
    let gpu = GpuSearcher::new(device, words)?;
    println!("[+] Copied to the card and compiled the kernel in {:.1?}", started.elapsed());
    Ok(gpu)
}

/// Checks the card against the CPU for a set of nonces, then measures its hashrate.
fn benchmark(gpu: &mut GpuSearcher, pow: &MoneyPrinterPow, batch: usize) -> Result<(), Error> {
    let header = Hash::from_bytes([0x01; 32]);
    for i in 0..32u64 {
        let nonce = i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (i << 7);
        let expected = pow.calculate_hash(&header, nonce);

        // A target equal to the hash must accept the nonce
        gpu.set_work(&header, &expected.0)?;
        if gpu.search(nonce, 1)? != vec![nonce] {
            return Err(format!("the card's hash for nonce {} differs from the CPU's", nonce).into());
        }
        // A target one below the hash must reject it
        let mut below = expected.0;
        for byte in below.iter_mut().rev() {
            let (value, borrow) = byte.overflowing_sub(1);
            *byte = value;
            if !borrow {
                break;
            }
        }
        gpu.set_work(&header, &below)?;
        if !gpu.search(nonce, 1)?.is_empty() {
            return Err(format!("the card accepted nonce {} against a target below its hash", nonce).into());
        }
    }
    println!("[+] The card's hashes match the CPU's for 32 nonces");

    // An unreachable target, so every batch runs in full
    gpu.set_work(&header, &[0u8; 32])?;
    gpu.search(0, batch)?;
    let started = Instant::now();
    let mut hashes = 0u64;
    let mut nonce = 0u64;
    while started.elapsed() < Duration::from_secs(10) {
        gpu.search(nonce, batch)?;
        nonce += batch as u64;
        hashes += batch as u64;
    }
    println!("[+] {:.2} MH/s ({} hashes in {:.1?})", hashes as f64 / started.elapsed().as_secs_f64() / 1e6, hashes, started.elapsed());
    Ok(())
}

/// The work a Stratum server last sent.
#[derive(Default)]
struct StratumWork {
    job_id: String,
    pre_pow_hash: Option<Hash>,
    target: Option<[u8; 32]>,
    /// Counts jobs and target changes, so the search loop notices new work.
    version: u64,
    accepted: u64,
    rejected: u64,
    disconnected: bool,
}

/// Mines through a Stratum bridge or pool (see docs/STRATUM.md).
fn mine_stratum(device: &Device, server: &str, args: &Args, batch: usize) -> Result<(), Error> {
    let address = args.address.as_deref().ok_or("--stratum needs --address, the address to be paid")?;
    let stream = TcpStream::connect(server).map_err(|e| format!("could not connect to {}: {}", server, e))?;
    stream.set_nodelay(true)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut send = |message: Value| -> Result<(), Error> {
        let mut line = message.to_string();
        line.push('\n');
        Ok(writer.write_all(line.as_bytes())?)
    };
    let read = |reader: &mut BufReader<TcpStream>| -> Result<Value, Error> {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err("the server closed the connection".into());
        }
        Ok(serde_json::from_str(&line)?)
    };

    send(json!({ "id": 1, "method": "mining.subscribe", "params": [concat!("imoney-gpu-miner/", env!("CARGO_PKG_VERSION"))] }))?;
    let subscribed = read(&mut reader)?;
    let result = &subscribed["result"];
    let extranonce = result[1]
        .as_str()
        .and_then(|hex| u16::from_str_radix(hex, 16).ok())
        .ok_or("the server did not assign an extranonce")?;
    let network = &result[3];
    let genesis = Hash::from_hex(network["genesis_hash"].as_str().ok_or("the server did not name its network")?)?;
    let params = PowParams {
        light_cache_items: network["light_cache_items"].as_u64().ok_or("the server did not give the cache size")? as u32,
        dataset_items: network["dataset_items"].as_u64().ok_or("the server did not give the dataset size")? as u32,
    };

    let pow = MoneyPrinterPow::new(params, genesis, PowMode::Full);
    let mut gpu = load(device, &pow)?;

    send(json!({ "id": 2, "method": "mining.authorize", "params": [address, "x"] }))?;
    println!("[+] Connected to {}", server);

    // A reader thread keeps the latest work; the search loop below picks it up between batches
    let work = Arc::new(Mutex::new(StratumWork::default()));
    {
        let work = work.clone();
        std::thread::spawn(move || {
            while let Ok(message) = read(&mut reader) {
                let mut work = work.lock().unwrap();
                match message["method"].as_str() {
                    Some("mining.set_target") => {
                        let target = message["params"][0].as_str().and_then(|hex| hex::decode(hex).ok());
                        work.target = target.and_then(|bytes| bytes.try_into().ok());
                        work.version += 1;
                    }
                    Some("mining.notify") => {
                        work.job_id = message["params"][0].as_str().unwrap_or_default().to_string();
                        work.pre_pow_hash = message["params"][1].as_str().and_then(|hex| Hash::from_hex(hex).ok());
                        work.version += 1;
                    }
                    // A reply: id 2 is the login, anything higher a share
                    _ => match message["id"].as_u64() {
                        Some(2) if !message["error"].is_null() => {
                            eprintln!("[-] Login refused: {}", message["error"][1].as_str().unwrap_or("no reason given"));
                            work.disconnected = true;
                            return;
                        }
                        Some(id) if id > 2 => {
                            if message["result"].as_bool() == Some(true) {
                                work.accepted += 1;
                            } else {
                                work.rejected += 1;
                            }
                        }
                        _ => {}
                    },
                }
            }
            work.lock().unwrap().disconnected = true;
        });
    }

    let started = Instant::now();
    let mut last_report = Instant::now();
    let mut hashes: u64 = 0;
    let mut next_id: u64 = 3;
    // This connection's nonces all start with the extranonce; the rest counts up
    let mut counter: u64 = rand::random::<u64>() & 0x0000_ffff_ffff_ffff;
    let mut loaded_version = 0;
    let mut current: Option<(String, Hash, [u8; 32])> = None;
    loop {
        {
            let work = work.lock().unwrap();
            if work.disconnected {
                return Err("the server closed the connection".into());
            }
            if work.version != loaded_version {
                loaded_version = work.version;
                if let (Some(pre_pow_hash), Some(target)) = (work.pre_pow_hash, work.target) {
                    gpu.set_work(&pre_pow_hash, &target)?;
                    current = Some((work.job_id.clone(), pre_pow_hash, target));
                }
            }
            if last_report.elapsed() > Duration::from_secs(10) {
                println!(
                    "[*] {:.2} MH/s | shares accepted {} rejected {}",
                    hashes as f64 / started.elapsed().as_secs_f64() / 1e6,
                    work.accepted,
                    work.rejected
                );
                last_report = Instant::now();
            }
        }
        if args.duration.is_some_and(|seconds| started.elapsed() >= Duration::from_secs(seconds)) {
            let work = work.lock().unwrap();
            println!(
                "[*] Finished: {} shares accepted, {} rejected, {:.2} MH/s average",
                work.accepted,
                work.rejected,
                hashes as f64 / started.elapsed().as_secs_f64() / 1e6
            );
            return Ok(());
        }
        let Some((job_id, pre_pow_hash, target)) = &current else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };

        let start_nonce = (extranonce as u64) << 48 | counter;
        let candidates = gpu.search(start_nonce, batch)?;
        counter = (counter + batch as u64) & 0x0000_ffff_ffff_ffff;
        hashes += batch as u64;
        for nonce in candidates {
            if pow.calculate_hash(pre_pow_hash, nonce).0 > *target {
                eprintln!("[-] The card reported nonce {} but the CPU rejects it; check the card and driver", nonce);
                continue;
            }
            send(json!({ "id": next_id, "method": "mining.submit", "params": [address, job_id, format!("{:016x}", nonce)] }))?;
            next_id += 1;
        }
    }
}

/// Mines for a node: fetch a block template, search for a nonce, submit, repeat.
fn mine(gpu: &mut GpuSearcher, pow: &MoneyPrinterPow, node: &str, args: &Args, batch: usize) -> Result<(), Error> {
    let template_url = match &args.address {
        Some(address) => format!("{}/api/v1/mining/template?address={}", node, address),
        None => format!("{}/api/v1/mining/template", node),
    };
    let submit_url = format!("{}/api/v1/mining/submit", node);

    let mut hashes: u64 = 0;
    let mut blocks_found: u64 = 0;
    let started = Instant::now();
    let mut last_report = Instant::now();
    loop {
        if args.duration.is_some_and(|seconds| started.elapsed() >= Duration::from_secs(seconds)) {
            println!(
                "[*] Finished: {} blocks accepted, {:.2} MH/s average",
                blocks_found,
                hashes as f64 / started.elapsed().as_secs_f64() / 1e6
            );
            return Ok(());
        }
        let template: Template = match ureq::get(&template_url).call() {
            Ok(response) => response.into_json()?,
            Err(e) => {
                eprintln!("[-] Could not fetch work: {}. Retrying...", e);
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let mut block = template.block;
        let pre_pow_hash = block.header.pre_pow_hash()?;
        gpu.set_work(&pre_pow_hash, &compact_to_u256(block.header.bits))?;

        let fetched = Instant::now();
        let mut nonce = rand::random::<u64>();
        let mut found = None;
        while found.is_none() && fetched.elapsed() < TEMPLATE_LIFETIME {
            let candidates = gpu.search(nonce, batch)?;
            nonce = nonce.wrapping_add(batch as u64);
            hashes += batch as u64;
            for candidate in candidates {
                if pow.verify(&pre_pow_hash, candidate, block.header.bits) {
                    found = Some(candidate);
                    break;
                }
                eprintln!("[-] The card reported nonce {} but the CPU rejects it; check the card and driver", candidate);
            }
        }

        if let Some(nonce) = found {
            block.header.nonce = nonce;
            let mut request = ureq::post(&submit_url);
            if let Some(token) = &args.rpc_token {
                request = request.set("Authorization", &format!("Bearer {}", token));
            }
            match request.send_json(serde_json::json!({ "block": block })) {
                Ok(response) => {
                    let result: SubmitResponse = response.into_json()?;
                    if result.success {
                        blocks_found += 1;
                        println!(
                            "[+] Block accepted: {} | DAA score {} | {} found so far",
                            result.block_hash.unwrap_or_default(),
                            block.header.daa_score,
                            blocks_found
                        );
                    }
                }
                // A stale template (another block arrived first) is rejected; that is normal
                Err(ureq::Error::Status(400, response)) => {
                    let result: SubmitResponse = response.into_json()?;
                    println!("[*] Block not accepted: {}", result.error.unwrap_or_default());
                }
                Err(ureq::Error::Status(401, _)) => return Err("the node requires --rpc-token".into()),
                Err(e) => eprintln!("[-] Could not submit block: {}", e),
            }
        }

        if last_report.elapsed() > Duration::from_secs(10) {
            println!("[*] {:.2} MH/s", hashes as f64 / started.elapsed().as_secs_f64() / 1e6);
            last_report = Instant::now();
        }
    }
}
