// SPDX-License-Identifier: MIT

use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use log::LevelFilter;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;

use crate::frb_generated::StreamSink;
use crate::node::Node;

static NODE: OnceLock<Node> = OnceLock::new();
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn rt() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to create tokio runtime")
    })
}

/// Initialize platform-specific logging.
fn init_logging() {
    #[cfg(target_os = "android")]
    {
        use android_logger::{Config, FilterBuilder};
        android_logger::init_once(
            Config::default()
                .with_max_level(LevelFilter::Trace)
                .with_filter(
                    FilterBuilder::new()
                        .filter(Some("rust_lib_panda_playground"), LevelFilter::Debug)
                        .filter(Some("p2panda"), LevelFilter::Info)
                        .filter(Some("iroh"), LevelFilter::Warn)
                        .build(),
                ),
        );
    }

    #[cfg(target_os = "ios")]
    {
        oslog::OsLogger::new("org.p2panda.playground")
            .level_filter(LevelFilter::Info)
            .init()
            .ok();
    }
}

/// Start the p2panda node with persistent identity stored in `data_dir`.
///
/// Returns this node's short ID (first 8 chars of public key).
pub fn start_node(data_dir: String) -> Result<String> {
    init_logging();
    log::info!("[api] start_node called with data_dir: {data_dir}");

    let node = Node::new();

    let (tx, rx) = oneshot::channel();

    rt().spawn(async move {
        let result = node.start(&data_dir).await;
        let _ = tx.send((node, result));
    });

    let (node, result) = rx
        .blocking_recv()
        .map_err(|_| anyhow!("node startup task failed"))?;

    let public_key = result.map_err(|e| anyhow!("node start failed: {e}"))?;
    let short_id = public_key[..8].to_string();

    NODE.set(node)
        .map_err(|_| anyhow!("node already started"))?;

    log::info!("[api] node started — ID: {short_id}, full key: {public_key}");
    Ok(short_id)
}

/// Get this node's full public key hex string (64 chars) for sharing with peers.
pub fn get_full_node_id() -> Result<String> {
    let node = NODE.get().ok_or_else(|| anyhow!("node not started"))?;

    let (tx, rx) = oneshot::channel();
    rt().spawn(async move {
        let _ = tx.send(node.full_id().await);
    });

    rx.blocking_recv()
        .map_err(|_| anyhow!("task failed"))?
        .map_err(|e| anyhow!("{e}"))
}

/// Add a remote peer by their hex-encoded public key.
///
/// This enables cross-network connectivity via the relay server.
pub fn add_peer(node_id: String) -> Result<()> {
    log::info!("[api] add_peer called: {}", &node_id[..8.min(node_id.len())]);
    let node = NODE.get().ok_or_else(|| anyhow!("node not started"))?;

    let (tx, rx) = oneshot::channel();
    rt().spawn(async move {
        let _ = tx.send(node.add_peer(&node_id).await);
    });

    rx.blocking_recv()
        .map_err(|_| anyhow!("task failed"))?
        .map_err(|e| anyhow!("{e}"))
}

/// Send a chat message. Broadcast to all nearby devices via gossip.
pub fn send_message(message: String) -> Result<()> {
    log::info!("[api] send_message called: '{message}'");
    let node = NODE.get().ok_or_else(|| anyhow!("node not started"))?;

    let (tx, rx) = oneshot::channel();
    let short_id_fut = {
        let node = node;
        async move {
            let short_id = node.short_id().await?;
            let payload = format!("{short_id}:{message}");
            log::info!("[api] sending payload: '{payload}'");
            node.publish("chat", payload.into_bytes()).await?;
            Ok::<_, crate::node::NodeError>(())
        }
    };

    rt().spawn(async move {
        let _ = tx.send(short_id_fut.await);
    });

    rx.blocking_recv()
        .map_err(|_| anyhow!("send task failed"))?
        .map_err(|e| anyhow!("{e}"))
}

/// Subscribe to incoming chat messages from nearby devices.
///
/// Messages arrive as "sender_id:message_text" strings via the StreamSink.
pub fn subscribe_chat(sink: StreamSink<String>) -> Result<()> {
    log::info!("[api] subscribe_chat called");
    let node = NODE.get().ok_or_else(|| anyhow!("node not started"))?;

    let (tx, rx) = oneshot::channel();

    let node_ref = node;
    rt().spawn(async move {
        match node_ref.subscribe("chat").await {
            Ok(mut receiver) => {
                log::info!("[api] chat subscription established, waiting for messages...");
                let _ = tx.send(Ok(()));
                while let Some(bytes) = receiver.recv().await {
                    match String::from_utf8(bytes) {
                        Ok(message) => {
                            log::info!("[api] forwarding to Dart: '{message}'");
                            if sink.add(message).is_err() {
                                log::warn!("[api] StreamSink closed, ending subscription");
                                break;
                            }
                        }
                        Err(e) => {
                            log::warn!("[api] received non-UTF8 message: {e}");
                        }
                    }
                }
                log::warn!("[api] chat receiver ended (mpsc channel closed)");
            }
            Err(e) => {
                log::error!("[api] subscribe failed: {e}");
                let _ = tx.send(Err(anyhow!("{e}")));
            }
        }
    });

    rx.blocking_recv()
        .map_err(|_| anyhow!("subscribe task failed"))?
}

/// Send a sketch stroke. Broadcast to all nearby devices via gossip.
///
/// Payload format: "sender_id:color:x1,y1;x2,y2;..."
pub fn send_sketch(stroke: String) -> Result<()> {
    log::info!("[api] send_sketch called ({} bytes)", stroke.len());
    let node = NODE.get().ok_or_else(|| anyhow!("node not started"))?;

    let (tx, rx) = oneshot::channel();
    let node_ref = node;
    rt().spawn(async move {
        let result = async {
            let short_id = node_ref.short_id().await?;
            let payload = format!("{short_id}:{stroke}");
            node_ref.publish("sketch", payload.into_bytes()).await?;
            Ok::<_, crate::node::NodeError>(())
        }
        .await;
        let _ = tx.send(result);
    });

    rx.blocking_recv()
        .map_err(|_| anyhow!("send task failed"))?
        .map_err(|e| anyhow!("{e}"))
}

/// Subscribe to incoming sketch strokes from nearby devices.
///
/// Strokes arrive as "sender_id:color:x1,y1;x2,y2;..." strings.
pub fn subscribe_sketch(sink: StreamSink<String>) -> Result<()> {
    log::info!("[api] subscribe_sketch called");
    let node = NODE.get().ok_or_else(|| anyhow!("node not started"))?;

    let (tx, rx) = oneshot::channel();
    let node_ref = node;
    rt().spawn(async move {
        match node_ref.subscribe("sketch").await {
            Ok(mut receiver) => {
                log::info!("[api] sketch subscription established");
                let _ = tx.send(Ok(()));
                while let Some(bytes) = receiver.recv().await {
                    if let Ok(message) = String::from_utf8(bytes) {
                        log::debug!("[api] sketch stroke received ({} bytes)", message.len());
                        if sink.add(message).is_err() {
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                log::error!("[api] sketch subscribe failed: {e}");
                let _ = tx.send(Err(anyhow!("{e}")));
            }
        }
    });

    rx.blocking_recv()
        .map_err(|_| anyhow!("subscribe task failed"))?
}

/// Shut down the node.
pub fn stop_node() -> Result<()> {
    let node = NODE.get().ok_or_else(|| anyhow!("node not started"))?;

    let (tx, rx) = oneshot::channel();
    rt().spawn(async move {
        let _ = tx.send(node.shutdown().await);
    });

    rx.blocking_recv()
        .map_err(|_| anyhow!("shutdown task failed"))?
        .map_err(|e| anyhow!("{e}"))
}
