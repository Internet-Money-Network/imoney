//! Internet Money for the desktop: starts a node in the background and shows its wallet.
//!
//! The node is a separate program bundled with the app (`binaries/imoney-node-<target>`). It
//! listens on this machine only, on ports of its own so it never clashes with a node the user
//! already runs, and keeps its data in the app's data folder. The window shows "Getting ready"
//! until the node answers, then the wallet page the node serves. That page gets no access to
//! the app: it is an ordinary web page, as in a browser.

// No console window behind the app on Windows
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{Manager, RunEvent, Url};

/// The built-in node's API, on this machine only.
const API_PORT: u16 = 18656;
/// Where it listens for other nodes. On this machine only: a wallet needs only to connect out,
/// and listening on the network would make Windows ask about its firewall on first run.
const P2P_PORT: u16 = 18655;
/// The public test network's seed nodes.
const SEEDS: &str = "seed.internetmoneynetwork.org:18555,seed1.internetmoneynetwork.org:18555,seed2.internetmoneynetwork.org:18555,seed3.internetmoneynetwork.org:18555";
/// How long the node has to start before the app gives up.
const START_TIMEOUT: Duration = Duration::from_secs(180);

/// The node, so it can be stopped when the app closes.
struct NodeProcess(Mutex<Option<Child>>);

fn node_program() -> PathBuf {
    // Tauri puts bundled programs beside the app, without the platform suffix
    let exe = std::env::current_exe().expect("the app knows where it is");
    let dir = exe.parent().expect("the app is in a folder");
    dir.join(if cfg!(windows) { "imoney-node.exe" } else { "imoney-node" })
}

fn start_node(data_dir: &Path) -> std::io::Result<Child> {
    std::fs::create_dir_all(data_dir)?;
    let mut command = Command::new(node_program());
    command
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--rpc-bind")
        .arg(format!("127.0.0.1:{}", API_PORT))
        .arg("--p2p-bind")
        .arg(format!("127.0.0.1:{}", P2P_PORT))
        .arg("--peers")
        .arg(SEEDS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window for the node either
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn()
}

/// Waits until the node's API answers, or the time runs out.
fn wait_for_node() -> bool {
    let started = Instant::now();
    let info = format!("http://127.0.0.1:{}/api/v1/info", API_PORT);
    while started.elapsed() < START_TIMEOUT {
        if ureq::get(&info).timeout(Duration::from_secs(2)).call().is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

fn stop_node(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<NodeProcess>() {
        if let Some(mut child) = state.0.lock().expect("not poisoned").take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn main() {
    let app = tauri::Builder::default()
        // Opening the app again brings this window forward instead of starting a second node
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?.join("node");
            let child = start_node(&data_dir).map_err(|e| format!("could not start the built-in node: {}", e))?;
            app.manage(NodeProcess(Mutex::new(Some(child))));

            let window = app.get_webview_window("main").expect("the main window is configured");
            std::thread::spawn(move || {
                if wait_for_node() {
                    let wallet: Url = format!("http://127.0.0.1:{}/wallet", API_PORT).parse().expect("a valid address");
                    let _ = window.navigate(wallet);
                } else {
                    let _ = window.eval(
                        "document.querySelector('h1').textContent = 'Could not start';\
                         document.querySelector('p').textContent = 'Close the app and open it again. If it keeps happening, restart your computer.';",
                    );
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("the app could not be set up");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            stop_node(app);
        }
    });
}
