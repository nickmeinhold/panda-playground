// SPDX-License-Identifier: MIT

use std::sync::Arc;

use futures_util::StreamExt;
use p2panda_core::Hash;
use p2panda_net::iroh_mdns::MdnsDiscoveryMode;
use p2panda_net::{AddressBook, Discovery, Endpoint, Gossip, MdnsDiscovery, TopicId};
use thiserror::Error;
use tokio::sync::RwLock;

/// Network identifier for Panda Playground.
fn network_id() -> Hash {
    Hash::new(b"panda-playground")
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
///
/// Manages gossip-based pub/sub for ephemeral messages between nearby devices.
pub struct Node {
    inner: Arc<RwLock<Option<NodeInner>>>,
}

struct NodeInner {
    _endpoint: Endpoint,
    _mdns: MdnsDiscovery,
    _discovery: Discovery,
    gossip: Gossip,
    public_key_hex: String,
}

impl Node {
    pub fn new() -> Self {
        Node {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// Start the node and begin discovering peers via mDNS.
    pub async fn start(&self) -> Result<String, NodeError> {
        let mut inner = self.inner.write().await;
        if inner.is_some() {
            return Err(NodeError::AlreadyRunning);
        }

        let address_book = AddressBook::builder()
            .spawn()
            .await
            .map_err(|e| NodeError::Network(format!("address book: {e}")))?;

        let endpoint = Endpoint::builder(address_book.clone())
            .network_id(network_id().into())
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
            gossip,
            public_key_hex: pk,
        });

        log::info!("Panda Playground node started");
        Ok(public_key_hex)
    }

    /// Publish a message to a named topic via gossip.
    pub async fn publish(&self, topic_name: &str, message: Vec<u8>) -> Result<(), NodeError> {
        let inner = self.inner.read().await;
        let inner = inner.as_ref().ok_or(NodeError::NotRunning)?;

        let topic = topic_from_name(topic_name);
        let stream = inner
            .gossip
            .stream(topic)
            .await
            .map_err(|e| NodeError::Network(format!("gossip stream: {e}")))?;

        stream
            .publish(message)
            .await
            .map_err(|e| NodeError::Network(format!("publish: {e}")))?;

        Ok(())
    }

    /// Subscribe to a named topic. Calls the provided function for each received message.
    pub async fn subscribe(
        &self,
        topic_name: &str,
    ) -> Result<tokio::sync::mpsc::Receiver<Vec<u8>>, NodeError> {
        let inner = self.inner.read().await;
        let inner = inner.as_ref().ok_or(NodeError::NotRunning)?;

        let topic = topic_from_name(topic_name);
        let stream = inner
            .gossip
            .stream(topic)
            .await
            .map_err(|e| NodeError::Network(format!("gossip stream: {e}")))?;

        let mut rx = stream.subscribe();
        let (tx, out_rx) = tokio::sync::mpsc::channel(256);

        tokio::spawn(async move {
            while let Some(Ok(bytes)) = rx.next().await {
                if tx.send(bytes).await.is_err() {
                    break;
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
