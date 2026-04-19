// SPDX-License-Identifier: MIT

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use futures_util::StreamExt;
use p2panda_core::{Hash, PrivateKey};
use p2panda_net::addrs::NodeInfo;
use p2panda_net::gossip::GossipHandle;
use p2panda_net::iroh_endpoint::{EndpointAddr, RelayUrl, from_public_key};
use p2panda_net::iroh_mdns::MdnsDiscoveryMode;
use p2panda_net::{AddressBook, Discovery, Endpoint, Gossip, MdnsDiscovery, TopicId};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};

const RELAY_URL: &str = "https://euc1-1.relay.n0.iroh-canary.iroh.link.";

/// Network identifier for Panda Playground.
fn network_id() -> Hash {
    Hash::new(b"panda-playground")
}

fn relay_url() -> Result<RelayUrl, NodeError> {
    RELAY_URL
        .parse()
        .map_err(|e| NodeError::Network(format!("relay url: {e}")))
}

/// Load or generate a persistent private key in the given directory.
fn load_or_create_key(data_dir: &Path) -> Result<PrivateKey, NodeError> {
    let key_path = data_dir.join("node_key");

    if key_path.exists() {
        let bytes = std::fs::read(&key_path)
            .map_err(|e| NodeError::Network(format!("read key: {e}")))?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| NodeError::Network("corrupt key file (expected 32 bytes)".into()))?;
        log::info!("[node] loaded existing private key from {}", key_path.display());
        Ok(PrivateKey::from_bytes(&bytes))
    } else {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| NodeError::Network(format!("create data dir: {e}")))?;
        let key = PrivateKey::new();
        std::fs::write(&key_path, key.as_bytes())
            .map_err(|e| NodeError::Network(format!("write key: {e}")))?;
        log::info!("[node] generated new private key at {}", key_path.display());
        Ok(key)
    }
}

/// Derive a topic ID from a string name.
pub fn topic_from_name(name: &str) -> TopicId {
    let hash = Hash::new(format!("panda-playground/{name}").as_bytes());
    let bytes: [u8; 32] = *hash.as_bytes();
    TopicId::from(bytes)
}

#[derive(Debug, Error)]
pub enum NodeError {
    #[error("network error: {0}")]
    Network(String),

    #[error("node already running")]
    AlreadyRunning,

    #[error("node not running")]
    NotRunning,
}

/// The Panda Playground p2panda node.
pub struct Node {
    inner: Arc<RwLock<Option<NodeInner>>>,
}

struct NodeInner {
    _endpoint: Endpoint,
    _mdns: MdnsDiscovery,
    _discovery: Discovery,
    address_book: AddressBook,
    gossip: Gossip,
    public_key_hex: String,
    /// Cache of gossip stream handles — one per topic, reused for both publish and subscribe.
    streams: Mutex<HashMap<TopicId, GossipHandle>>,
}

impl NodeInner {
    /// Get or create a gossip stream for a topic.
    async fn get_stream(&self, topic: TopicId) -> Result<GossipHandle, NodeError> {
        let topic_hex = format!("{topic:?}");
        let mut streams = self.streams.lock().await;
        if let Some(handle) = streams.get(&topic) {
            log::debug!("[gossip] reusing cached handle for topic {topic_hex}");
            return Ok(handle.clone());
        }
        log::info!("[gossip] creating new handle for topic {topic_hex}");
        let handle = self
            .gossip
            .stream(topic)
            .await
            .map_err(|e| NodeError::Network(format!("gossip stream: {e}")))?;
        streams.insert(topic, handle.clone());
        Ok(handle)
    }
}

impl Node {
    pub fn new() -> Self {
        Node {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// Start the node with a persistent identity stored in `data_dir`.
    pub async fn start(&self, data_dir: &str) -> Result<String, NodeError> {
        let mut inner = self.inner.write().await;
        if inner.is_some() {
            return Err(NodeError::AlreadyRunning);
        }

        let private_key = load_or_create_key(Path::new(data_dir))?;

        let address_book = AddressBook::builder()
            .spawn()
            .await
            .map_err(|e| NodeError::Network(format!("address book: {e}")))?;

        let endpoint = Endpoint::builder(address_book.clone())
            .network_id(network_id().into())
            .private_key(private_key)
            .relay_url(relay_url()?)
            .spawn()
            .await
            .map_err(|e| NodeError::Network(format!("endpoint: {e}")))?;

        let public_key_hex = hex::encode(endpoint.node_id().as_bytes());

        let mdns = MdnsDiscovery::builder(address_book.clone(), endpoint.clone())
            .mode(MdnsDiscoveryMode::Active)
            .spawn()
            .await
            .map_err(|e| NodeError::Network(format!("mdns: {e}")))?;

        let discovery = Discovery::builder(address_book.clone(), endpoint.clone())
            .spawn()
            .await
            .map_err(|e| NodeError::Network(format!("discovery: {e}")))?;

        let gossip = Gossip::builder(address_book.clone(), endpoint.clone())
            .spawn()
            .await
            .map_err(|e| NodeError::Network(format!("gossip: {e}")))?;

        let pk = public_key_hex.clone();

        *inner = Some(NodeInner {
            _endpoint: endpoint,
            _mdns: mdns,
            _discovery: discovery,
            address_book,
            gossip,
            public_key_hex: pk,
            streams: Mutex::new(HashMap::new()),
        });

        log::info!("Panda Playground node started");
        Ok(public_key_hex)
    }

    /// Publish a message to a named topic via gossip.
    pub async fn publish(&self, topic_name: &str, message: Vec<u8>) -> Result<(), NodeError> {
        let inner = self.inner.read().await;
        let inner = inner.as_ref().ok_or(NodeError::NotRunning)?;

        let topic = topic_from_name(topic_name);
        let stream = inner.get_stream(topic).await?;

        log::info!(
            "[gossip] publishing {} bytes to topic '{topic_name}'",
            message.len()
        );
        stream
            .publish(message)
            .await
            .map_err(|e| NodeError::Network(format!("publish: {e}")))?;

        log::debug!("[gossip] publish succeeded on '{topic_name}'");
        Ok(())
    }

    /// Subscribe to a named topic. Returns a receiver for incoming messages.
    pub async fn subscribe(
        &self,
        topic_name: &str,
    ) -> Result<tokio::sync::mpsc::Receiver<Vec<u8>>, NodeError> {
        let inner = self.inner.read().await;
        let inner = inner.as_ref().ok_or(NodeError::NotRunning)?;

        let topic = topic_from_name(topic_name);
        let stream = inner.get_stream(topic).await?;

        let mut rx = stream.subscribe();
        let (tx, out_rx) = tokio::sync::mpsc::channel(256);

        let name = topic_name.to_string();
        log::info!("[gossip] subscription started for topic '{name}'");

        tokio::spawn(async move {
            loop {
                match rx.next().await {
                    Some(Ok(bytes)) => {
                        log::info!(
                            "[gossip] received {} bytes on topic '{name}'",
                            bytes.len()
                        );
                        if tx.send(bytes).await.is_err() {
                            log::warn!("[gossip] subscriber channel closed for '{name}'");
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        log::warn!("[gossip] receive error on '{name}': {e}");
                    }
                    None => {
                        log::warn!("[gossip] subscription stream ended for '{name}'");
                        break;
                    }
                }
            }
        });

        Ok(out_rx)
    }

    /// Get this node's public key (short form for display).
    pub async fn short_id(&self) -> Result<String, NodeError> {
        let inner = self.inner.read().await;
        let inner = inner.as_ref().ok_or(NodeError::NotRunning)?;
        Ok(inner.public_key_hex[..8].to_string())
    }

    /// Get this node's full public key hex string (for sharing with peers).
    pub async fn full_id(&self) -> Result<String, NodeError> {
        let inner = self.inner.read().await;
        let inner = inner.as_ref().ok_or(NodeError::NotRunning)?;
        Ok(inner.public_key_hex.clone())
    }

    /// Add a remote peer by their hex-encoded public key.
    ///
    /// The peer is added to the address book as a bootstrap node with the relay URL,
    /// enabling connection across different networks.
    pub async fn add_peer(&self, node_id_hex: &str) -> Result<(), NodeError> {
        let inner = self.inner.read().await;
        let inner = inner.as_ref().ok_or(NodeError::NotRunning)?;

        let public_key: p2panda_core::PublicKey = node_id_hex
            .parse()
            .map_err(|e| NodeError::Network(format!("invalid node ID: {e}")))?;

        let endpoint_addr =
            EndpointAddr::new(from_public_key(public_key)).with_relay_url(relay_url()?);
        let node_info = NodeInfo::from(endpoint_addr).bootstrap();

        inner
            .address_book
            .insert_node_info(node_info)
            .await
            .map_err(|e| NodeError::Network(format!("insert peer: {e}")))?;

        log::info!("[node] added peer {}", &node_id_hex[..8]);
        Ok(())
    }

    /// Shut down the node.
    pub async fn shutdown(&self) -> Result<(), NodeError> {
        let mut inner = self.inner.write().await;
        if inner.is_none() {
            return Err(NodeError::NotRunning);
        }
        *inner = None;
        log::info!("Panda Playground node shut down");
        Ok(())
    }
}
