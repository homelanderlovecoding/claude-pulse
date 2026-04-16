use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, RwLock};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "pulse-daemon", about = "Pulse host daemon – bridges Claude Code with the button")]
struct Cli {
    /// WebSocket listen port
    #[arg(long, default_value_t = 3456)]
    port: u16,

    /// Path to the state file written by Claude Code hooks
    #[arg(long, default_value = default_state_file())]
    state_file: PathBuf,
}

fn default_state_file() -> &'static str {
    // We resolve ~ at runtime, but clap needs a &str default.
    "~/.pulse/state.json"
}

fn resolve_tilde(p: &PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if s.starts_with("~/") {
        if let Some(home) = dirs_home() {
            return home.join(&s[2..]);
        }
    }
    p.clone()
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct PulseState {
    state: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ButtonAction {
    action: String,
    #[serde(default)]
    detail: String,
}

// ---------------------------------------------------------------------------
// Shared daemon state
// ---------------------------------------------------------------------------

struct DaemonState {
    current: RwLock<PulseState>,
    tx: broadcast::Sender<String>, // broadcast JSON strings to WS clients
}

// ---------------------------------------------------------------------------
// State-file watcher
// ---------------------------------------------------------------------------

async fn watch_state_file(
    state_file: PathBuf,
    daemon: Arc<DaemonState>,
) {
    // Ensure parent dir + initial file exist
    if let Some(parent) = state_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if !state_file.exists() {
        let initial = PulseState {
            state: "idle".into(),
            message: "Daemon started".into(),
        };
        let json = serde_json::to_string_pretty(&initial).unwrap();
        let _ = std::fs::write(&state_file, &json);
        info!("Created initial state file at {}", state_file.display());
    }

    // Read the current contents once at startup
    if let Ok(contents) = std::fs::read_to_string(&state_file) {
        if let Ok(ps) = serde_json::from_str::<PulseState>(&contents) {
            *daemon.current.write().await = ps;
        }
    }

    // Set up file watcher
    let (fs_tx, mut fs_rx) = tokio::sync::mpsc::channel::<()>(16);

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_)
                ) {
                    let _ = fs_tx.blocking_send(());
                }
            }
        },
        notify::Config::default(),
    )
    .expect("Failed to create file watcher");

    // Watch the parent directory (works better across OSes for single-file watching)
    let watch_dir = state_file.parent().unwrap_or(&state_file);
    watcher
        .watch(watch_dir.as_ref(), RecursiveMode::NonRecursive)
        .expect("Failed to watch state directory");

    info!("Watching state file: {}", state_file.display());

    while fs_rx.recv().await.is_some() {
        // Small debounce – file writes may trigger multiple events
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        // drain any queued signals
        while fs_rx.try_recv().is_ok() {}

        match std::fs::read_to_string(&state_file) {
            Ok(contents) => match serde_json::from_str::<PulseState>(&contents) {
                Ok(ps) => {
                    let changed = {
                        let current = daemon.current.read().await;
                        *current != ps
                    };
                    if changed {
                        info!("State changed → {} : {}", ps.state, ps.message);
                        *daemon.current.write().await = ps.clone();
                        let json = serde_json::to_string(&ps).unwrap();
                        let _ = daemon.tx.send(json);
                    }
                }
                Err(e) => warn!("Failed to parse state file: {e}"),
            },
            Err(e) => warn!("Failed to read state file: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Action handler
// ---------------------------------------------------------------------------

fn actions_log_path(state_file: &PathBuf) -> PathBuf {
    state_file
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("actions.log")
}

fn learn_json_path(state_file: &PathBuf) -> PathBuf {
    state_file
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("learn.json")
}

fn handle_action(action: &ButtonAction, state_file: &PathBuf) {
    let ts = timestamp_now();
    let log_line = format!("[{ts}] action={} detail={}\n", action.action, action.detail);

    // Append to actions.log
    let log_path = actions_log_path(state_file);
    if let Err(e) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, log_line.as_bytes()))
    {
        error!("Failed to write actions.log: {e}");
    }

    // Action-specific behaviour
    match action.action.as_str() {
        "approve" => {
            info!("Button: APPROVE");
        }
        "reject_learn" => {
            info!("Button: REJECT + LEARN");
            let entry = serde_json::json!({
                "timestamp": ts,
                "action": "reject_learn",
                "detail": action.detail,
            });
            let learn_path = learn_json_path(state_file);
            let mut entries: Vec<serde_json::Value> = std::fs::read_to_string(&learn_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            entries.push(entry);
            let _ = std::fs::write(&learn_path, serde_json::to_string_pretty(&entries).unwrap());
        }
        "security_scan" => {
            info!("Button: SECURITY SCAN (placeholder)");
        }
        "explain" => {
            info!("Button: EXPLAIN");
        }
        other => {
            warn!("Unknown action: {other}");
        }
    }

    // Always print to stdout for debugging
    println!("{}", log_line.trim_end());
}

fn timestamp_now() -> String {
    // Simple UTC timestamp without pulling in chrono
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Format as seconds-since-epoch (good enough; avoids extra dep)
    format!("{secs}")
}

// ---------------------------------------------------------------------------
// WebSocket server
// ---------------------------------------------------------------------------

async fn handle_connection(
    stream: TcpStream,
    addr: std::net::SocketAddr,
    daemon: Arc<DaemonState>,
    state_file: PathBuf,
) {
    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            error!("WebSocket handshake failed for {addr}: {e}");
            return;
        }
    };

    info!("Client connected: {addr}");

    let (mut ws_sink, mut ws_source) = ws_stream.split();

    // Send current state immediately on connect
    {
        let current = daemon.current.read().await;
        let json = serde_json::to_string(&*current).unwrap();
        if let Err(e) = ws_sink.send(Message::Text(json.into())).await {
            error!("Failed to send initial state to {addr}: {e}");
            return;
        }
    }

    // Subscribe to broadcast updates
    let mut rx = daemon.tx.subscribe();

    loop {
        tokio::select! {
            // Broadcast state changes → client
            msg = rx.recv() => {
                match msg {
                    Ok(json) => {
                        if let Err(e) = ws_sink.send(Message::Text(json.into())).await {
                            warn!("Send to {addr} failed: {e}");
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            // Client messages → action handler
            msg = ws_source.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ButtonAction>(&text) {
                            Ok(action) => handle_action(&action, &state_file),
                            Err(e) => warn!("Bad action JSON from {addr}: {e}"),
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        info!("Client disconnected: {addr}");
                        break;
                    }
                    Some(Err(e)) => {
                        warn!("WS error from {addr}: {e}");
                        break;
                    }
                    _ => {} // ping/pong/binary – ignore
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let state_file = resolve_tilde(&cli.state_file);

    info!("pulse-daemon v{}", env!("CARGO_PKG_VERSION"));
    info!("WebSocket port : {}", cli.port);
    info!("State file     : {}", state_file.display());

    let (tx, _) = broadcast::channel::<String>(64);

    let daemon = Arc::new(DaemonState {
        current: RwLock::new(PulseState {
            state: "idle".into(),
            message: String::new(),
        }),
        tx,
    });

    // Spawn state-file watcher
    let d = daemon.clone();
    let sf = state_file.clone();
    tokio::spawn(async move {
        watch_state_file(sf, d).await;
    });

    // Bind WebSocket server
    let addr = format!("0.0.0.0:{}", cli.port);
    let listener = TcpListener::bind(&addr)
        .await
        .expect("Failed to bind WebSocket port");
    info!("Listening on ws://{addr}");

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                let d = daemon.clone();
                let sf = state_file.clone();
                tokio::spawn(async move {
                    handle_connection(stream, addr, d, sf).await;
                });
            }
            Err(e) => error!("Accept failed: {e}"),
        }
    }
}
