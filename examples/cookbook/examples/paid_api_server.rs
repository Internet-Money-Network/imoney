//! A web service that charges per request, with no accounts and no payment processor.
//!
//!     cargo run -p imoney-cookbook --example paid_api_server -- <address-to-be-paid-at>
//!
//! - `GET /quote` returns a price, the address to pay and a fresh invoice number.
//! - `GET /data?invoice=<number>` returns the data once that invoice is paid, and
//!   `402 Payment Required` until then.
//!
//! The service holds no key: it only needs an address to be paid at and a node to ask. It asks
//! its own node, so nobody else has to be trusted about whether the money arrived.

use imoney_cookbook::{node_url, Node};
use imoney_core::Address;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

/// 0.01 IMN per request.
const PRICE_ATOMS: u64 = 1_000_000;

fn respond(stream: &mut TcpStream, status: &str, body: serde_json::Value) {
    let body = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    );
}

fn main() -> Result<(), String> {
    let address = std::env::args().nth(1).ok_or("usage: paid_api_server <address-to-be-paid-at>")?;
    let address = Address::decode(&address).map_err(|e| format!("bad address: {}", e))?;
    let node = Node::new(&node_url());
    // Invoices this service issued, and those already used: one payment buys one answer
    let mut issued: HashSet<String> = HashSet::new();
    let mut served: HashSet<String> = HashSet::new();

    let listener = TcpListener::bind("127.0.0.1:8402").map_err(|e| e.to_string())?;
    println!("Paid API on http://127.0.0.1:8402, paid at {}", address);
    for mut stream in listener.incoming().flatten() {
        let mut request_line = String::new();
        if BufReader::new(&stream).read_line(&mut request_line).is_err() {
            continue;
        }
        let path = request_line.split_whitespace().nth(1).unwrap_or("/");

        if path == "/quote" {
            let invoice_id = format!("api-{}-{}", std::process::id(), issued.len() + 1);
            issued.insert(invoice_id.clone());
            respond(
                &mut stream,
                "200 OK",
                serde_json::json!({ "invoice_id": invoice_id, "address": address.to_string(), "price_atoms": PRICE_ATOMS }),
            );
        } else if let Some(invoice_id) = path.strip_prefix("/data?invoice=") {
            if !issued.contains(invoice_id) {
                respond(&mut stream, "404 Not Found", serde_json::json!({ "error": "unknown invoice" }));
                continue;
            }
            // "Seen" means a valid payment is waiting for a block, which takes under a second.
            // That is enough for a cheap request. For something valuable, wait for `included`
            // or `final` instead: an unconfirmed payment can still be replaced by its sender.
            let (seen_atoms, _included, _final) = match node.invoice_paid(invoice_id, &address) {
                Ok(paid) => paid,
                Err(e) => {
                    respond(&mut stream, "503 Service Unavailable", serde_json::json!({ "error": e }));
                    continue;
                }
            };
            if seen_atoms < PRICE_ATOMS {
                respond(
                    &mut stream,
                    "402 Payment Required",
                    serde_json::json!({ "error": "not paid yet", "paid_atoms": seen_atoms, "price_atoms": PRICE_ATOMS }),
                );
            } else if !served.insert(invoice_id.to_string()) {
                respond(&mut stream, "410 Gone", serde_json::json!({ "error": "this invoice was already used" }));
            } else {
                println!("Served {} for {} atoms", invoice_id, seen_atoms);
                respond(&mut stream, "200 OK", serde_json::json!({ "data": "the answer you paid for" }));
            }
        } else {
            respond(&mut stream, "404 Not Found", serde_json::json!({ "error": "try /quote" }));
        }
    }
    Ok(())
}
