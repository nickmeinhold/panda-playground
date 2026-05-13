// SPDX-License-Identifier: MIT

use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Result};
#[cfg(not(target_arch = "wasm32"))]
use audiopus::coder::{Decoder as OpusDecoder, Encoder as OpusEncoder};
#[cfg(not(target_arch = "wasm32"))]
use audiopus::packet::Packet as OpusPacket;
#[cfg(not(target_arch = "wasm32"))]
use audiopus::{Application, Channels, MutSignals, SampleRate};
#[cfg(any(target_os = "android", target_os = "ios"))]
use log::LevelFilter;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, oneshot};

use crate::frb_generated::StreamSink;
use crate::node::{GossipTopic, Node};

static NODE: OnceLock<Node> = OnceLock::new();
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
#[cfg(not(target_arch = "wasm32"))]
static OPUS_ENCODER: OnceLock<Mutex<OpusEncoder>> = OnceLock::new();
#[cfg(not(target_arch = "wasm32"))]
static OPUS_DECODER: OnceLock<Mutex<OpusDecoder>> = OnceLock::new();
/// Bounded channel feeding the voice publisher task. Capacity 10 ≈ 200ms of
/// frames at 50fps; `try_send` is non-blocking and drops the new frame on
/// overflow (drop-newest is acceptable for voice — sustained backpressure
/// can't be "caught up" in real time anyway).
#[cfg(not(target_arch = "wasm32"))]
static VOICE_TX: OnceLock<mpsc::Sender<Vec<u8>>> = OnceLock::new();
/// Cached 8-byte sender prefix derived from this node's short_id. Computed
/// once at voice-session start so per-frame send is fully synchronous.
#[cfg(not(target_arch = "wasm32"))]
static MY_VOICE_SENDER_BYTES: OnceLock<[u8; VOICE_SENDER_LEN]> = OnceLock::new();

/// Opus frame size: 320 samples = 20ms at 16kHz.
#[cfg(not(target_arch = "wasm32"))]
const OPUS_FRAME_SAMPLES: usize = 320;
/// Sender ID prefix length in voice messages.
#[cfg(not(target_arch = "wasm32"))]
const VOICE_SENDER_LEN: usize = 8;
/// Bounded voice publisher queue capacity. 10 frames at 50fps (20ms each)
/// is roughly 200ms of buffered audio; chosen as the smallest cap that
/// absorbs typical gossip rebroadcast jitter without blocking the mic
/// stream. Tune via empirical measurement under real mesh conditions.
#[cfg(not(target_arch = "wasm32"))]
const VOICE_QUEUE_CAPACITY: usize = 10;

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
    log::info!("[api] add_peer called ({} chars)", node_id.len());
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
            node.publish(GossipTopic::Chat, payload.into_bytes()).await?;
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
        match node_ref.subscribe(GossipTopic::Chat).await {
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
            node_ref.publish(GossipTopic::Sketch, payload.into_bytes()).await?;
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
        match node_ref.subscribe(GossipTopic::Sketch).await {
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

/// Initialize the Opus encoder and decoder for voice chat, plus the
/// long-running publisher task that drains the voice queue.
///
/// Idempotent: safe to call repeatedly. All process-global state lives in
/// `OnceLock`s and survives widget rebuilds, hot restarts, and tab
/// re-entries; subsequent calls are a no-op.
#[cfg(not(target_arch = "wasm32"))]
pub fn start_voice_session() -> Result<()> {
    if OPUS_ENCODER.get().is_some() && OPUS_DECODER.get().is_some() {
        log::info!("[api] start_voice_session: already initialized, no-op");
        return Ok(());
    }

    log::info!("[api] start_voice_session: initializing");

    let node = NODE.get().ok_or_else(|| anyhow!("node not started"))?;

    // Cache the 8-byte sender prefix once. `short_id()` is stable for a given
    // node (it's a slice of the public key hex), so per-frame send doesn't
    // need to repeat the async fetch.
    let short_id_str = rt()
        .block_on(async { node.short_id().await })
        .map_err(|e| anyhow!("short_id: {e}"))?;
    let bytes = short_id_str.as_bytes();
    if bytes.len() < VOICE_SENDER_LEN {
        return Err(anyhow!(
            "short_id is {} bytes, need at least {}",
            bytes.len(),
            VOICE_SENDER_LEN
        ));
    }
    let mut sender_bytes = [0u8; VOICE_SENDER_LEN];
    sender_bytes.copy_from_slice(&bytes[..VOICE_SENDER_LEN]);
    let _ = MY_VOICE_SENDER_BYTES.set(sender_bytes);

    let mut encoder = OpusEncoder::new(SampleRate::Hz16000, Channels::Mono, Application::Voip)
        .map_err(|e| anyhow!("opus encoder: {e}"))?;

    // Enable Discontinuous Transmission (DTX): when the encoder detects silence
    // it emits 1-2 byte "silence" frames instead of the usual 40-80 bytes. For
    // PTT use the win is small (you only encode while holding the button) but
    // it's correct behaviour and matters once voice moves to open-mic.
    encoder
        .enable_dtx()
        .map_err(|e| anyhow!("opus enable_dtx: {e}"))?;

    let decoder = OpusDecoder::new(SampleRate::Hz16000, Channels::Mono)
        .map_err(|e| anyhow!("opus decoder: {e}"))?;

    let _ = OPUS_ENCODER.set(Mutex::new(encoder));
    let _ = OPUS_DECODER.set(Mutex::new(decoder));

    // Spawn the voice publisher task. It runs for the lifetime of the process
    // (the channel sender never gets dropped — it's stored in a OnceLock),
    // pulling payloads off the bounded queue and publishing to gossip.
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(VOICE_QUEUE_CAPACITY);
    rt().spawn(async move {
        log::info!("[voice] publisher task started");
        while let Some(payload) = rx.recv().await {
            if let Err(e) = node.publish(GossipTopic::Voice, payload).await {
                log::warn!("[voice] publish failed: {e}");
            }
        }
        log::info!("[voice] publisher task ended (channel closed)");
    });
    let _ = VOICE_TX.set(tx);

    log::info!("[api] opus encoder/decoder initialized (16kHz mono, VOIP, DTX on)");
    Ok(())
}

/// Encode a PCM audio frame with Opus and enqueue it for the voice publisher
/// task. Fire-and-forget: the caller doesn't wait for gossip publish to
/// complete, so a 50fps mic stream isn't bottlenecked by per-frame round-trip.
///
/// `pcm_bytes` must be exactly 640 bytes of 16-bit signed LE PCM (320 samples
/// at 16kHz = 20ms).
///
/// If the publisher queue is full, the new frame is silently dropped —
/// voice tolerates loss far better than latency, and sustained backpressure
/// means real-time playback can't catch up anyway.
#[cfg(not(target_arch = "wasm32"))]
pub fn send_voice_frame(pcm_bytes: Vec<u8>) -> Result<()> {
    let encoder = OPUS_ENCODER
        .get()
        .ok_or_else(|| anyhow!("voice session not started"))?;
    let sender_bytes = MY_VOICE_SENDER_BYTES
        .get()
        .ok_or_else(|| anyhow!("voice session not started (sender prefix missing)"))?;
    let tx = VOICE_TX
        .get()
        .ok_or_else(|| anyhow!("voice session not started (publisher channel missing)"))?;

    // Validate raw byte length up front so an odd-length input is rejected
    // rather than silently truncated by chunks_exact(2).
    let expected_bytes = OPUS_FRAME_SAMPLES * 2;
    if pcm_bytes.len() != expected_bytes {
        return Err(anyhow!(
            "expected {} bytes of 16-bit LE PCM ({} samples at 16kHz mono = 20ms), got {} bytes",
            expected_bytes,
            OPUS_FRAME_SAMPLES,
            pcm_bytes.len()
        ));
    }

    let samples: Vec<i16> = pcm_bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    // Encode PCM → Opus
    let mut opus_buf = vec![0u8; 256]; // Opus frames are typically <100 bytes
    let encoded_len = {
        let enc = encoder.lock().map_err(|e| anyhow!("encoder lock: {e}"))?;
        enc.encode(&samples, &mut opus_buf)
            .map_err(|e| anyhow!("opus encode: {e}"))?
    };
    opus_buf.truncate(encoded_len);

    // Build payload: [8-byte sender prefix][Opus packet]
    let mut payload = Vec::with_capacity(VOICE_SENDER_LEN + opus_buf.len());
    payload.extend_from_slice(sender_bytes);
    payload.extend_from_slice(&opus_buf);

    // Fire and forget. try_send is non-blocking; if the queue is full we
    // drop the new frame and continue.
    match tx.try_send(payload) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            log::debug!("[voice] publisher queue full, dropping frame");
            Ok(())
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            Err(anyhow!("voice publisher task is no longer running"))
        }
    }
}

/// Subscribe to incoming voice frames from nearby devices.
///
/// Decoded PCM frames (640 bytes of 16-bit signed LE, 320 samples at 16kHz)
/// are streamed to the sink. Own frames are skipped.
#[cfg(not(target_arch = "wasm32"))]
pub fn subscribe_voice(sink: StreamSink<Vec<u8>>) -> Result<()> {
    log::info!("[api] subscribe_voice called");
    let node = NODE.get().ok_or_else(|| anyhow!("node not started"))?;
    let decoder = OPUS_DECODER
        .get()
        .ok_or_else(|| anyhow!("voice session not started"))?;

    let my_sender_bytes = MY_VOICE_SENDER_BYTES
        .get()
        .copied()
        .ok_or_else(|| anyhow!("voice session not started (sender prefix missing)"))?;

    let (tx, rx) = oneshot::channel();
    let node_ref = node;

    rt().spawn(async move {
        match node_ref.subscribe(GossipTopic::Voice).await {
            Ok(mut receiver) => {
                log::info!("[api] voice subscription established");
                let _ = tx.send(Ok(()));

                while let Some(bytes) = receiver.recv().await {
                    if bytes.len() <= VOICE_SENDER_LEN {
                        continue;
                    }

                    // Split sender ID and opus frame
                    let sender = &bytes[..VOICE_SENDER_LEN];
                    if sender == my_sender_bytes {
                        continue; // skip own voice
                    }

                    let opus_frame = &bytes[VOICE_SENDER_LEN..];

                    // Decode Opus → PCM
                    let mut pcm_samples = vec![0i16; OPUS_FRAME_SAMPLES];
                    let decoded_len = {
                        let mut dec = match decoder.lock() {
                            Ok(d) => d,
                            Err(e) => {
                                log::warn!("[api] decoder lock failed: {e}");
                                continue;
                            }
                        };
                        let packet = match OpusPacket::try_from(opus_frame) {
                            Ok(p) => p,
                            Err(e) => {
                                log::warn!("[api] invalid opus packet: {e}");
                                continue;
                            }
                        };
                        let output = match MutSignals::try_from(&mut pcm_samples) {
                            Ok(s) => s,
                            Err(e) => {
                                log::warn!("[api] signals error: {e}");
                                continue;
                            }
                        };
                        match dec.decode(Some(packet), output, false) {
                            Ok(len) => len,
                            Err(e) => {
                                log::warn!("[api] opus decode error: {e}");
                                continue;
                            }
                        }
                    };

                    // Convert i16 samples → bytes (LE)
                    let pcm_bytes: Vec<u8> = pcm_samples[..decoded_len]
                        .iter()
                        .flat_map(|s| s.to_le_bytes())
                        .collect();

                    if sink.add(pcm_bytes).is_err() {
                        log::warn!("[api] voice StreamSink closed");
                        break;
                    }
                }
                log::warn!("[api] voice receiver ended");
            }
            Err(e) => {
                log::error!("[api] voice subscribe failed: {e}");
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
