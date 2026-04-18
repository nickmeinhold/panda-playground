// SPDX-License-Identifier: MIT

use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use log::LevelFilter;
use tokio::runtime::Runtime;

use crate::node::Node;

static NODE: OnceLock<Node> = OnceLock::new();
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn rt() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
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
                        .filter(Some("panda_playground"), LevelFilter::Debug)
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

/// Start the p2panda node. Returns this node's short ID (first 8 chars of public key).
pub fn start_node() -> Result<String> {
    init_logging();

    let node = Node::new();
    let public_key = rt().block_on(node.start()).map_err(|e| anyhow!("{e}"))?;
    let short_id = public_key[..8].to_string();

    NODE.set(node)
        .map_err(|_| anyhow!("node already started"))?;

    log::info!("Node started with ID: {short_id}");
    Ok(short_id)
}

/// Send a chat message. Broadcast to all nearby devices via gossip.
pub fn send_message(message: String) -> Result<()> {
    let node = NODE.get().ok_or_else(|| anyhow!("node not started"))?;

    let short_id = rt().block_on(node.short_id()).map_err(|e| anyhow!("{e}"))?;
    let payload = format!("{short_id}:{message}");

    rt().block_on(node.publish("chat", payload.into_bytes()))
        .map_err(|e| anyhow!("{e}"))?;

    Ok(())
}

/// Shut down the node.
pub fn stop_node() -> Result<()> {
    let node = NODE.get().ok_or_else(|| anyhow!("node not started"))?;
    rt().block_on(node.shutdown()).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}
