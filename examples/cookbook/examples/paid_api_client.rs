//! A program that pays for an API call by itself: no person, no card, no account.
//!
//!     cargo run -p imoney-cookbook --example paid_api_client -- <key>
//!
//! It asks `paid_api_server` for a quote, pays the invoice, and collects the answer.

use imoney_cookbook::{address_of, imn, load_key, node_url, Node, FEE_ATOMS, NETWORK};
use imoney_core::{Address, Transaction};
use std::time::{Duration, Instant};

const SERVICE: &str = "http://127.0.0.1:8402";

/// A program that spends money by itself needs a limit it will not exceed.
const MOST_IT_WILL_PAY_ATOMS: u64 = 5_000_000;

fn main() -> Result<(), String> {
    let key = load_key(&std::env::args().nth(1).ok_or("usage: paid_api_client <key>")?)?;
    let node = Node::new(&node_url());
    let http = ureq::Agent::new();
    let started = Instant::now();

    // 1. Ask what it costs
    let quote: serde_json::Value = http
        .get(&format!("{}/quote", SERVICE))
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    let invoice_id = quote["invoice_id"].as_str().ok_or("no invoice in the quote")?;
    let address = Address::decode(quote["address"].as_str().ok_or("no address in the quote")?).map_err(|e| e.to_string())?;
    let price_atoms = quote["price_atoms"].as_u64().ok_or("no price in the quote")?;
    println!("Quoted {} IMN, invoice {}", imn(price_atoms), invoice_id);
    if price_atoms > MOST_IT_WILL_PAY_ATOMS {
        return Err(format!("{} IMN is more than this client is allowed to pay", imn(price_atoms)));
    }

    // 2. Pay the invoice. The invoice number is signed into the payment, so the service can
    //    tell this payment from every other one arriving at the same address.
    let coins = node.spendable_coins(&address_of(&key))?;
    let tx = Transaction::build_invoice_payment(&key, NETWORK, &address, price_atoms, FEE_ATOMS, coins, None, Some(invoice_id))?;
    println!("Paid in transaction {}", node.broadcast(&tx)?);

    // 3. Collect. The service's node hears of the payment within moments.
    loop {
        match http.get(&format!("{}/data?invoice={}", SERVICE, invoice_id)).call() {
            Ok(response) => {
                let answer = response.into_string().map_err(|e| e.to_string())?;
                println!("Answer after {:.1}s: {}", started.elapsed().as_secs_f32(), answer);
                return Ok(());
            }
            Err(ureq::Error::Status(402, _)) if started.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}
