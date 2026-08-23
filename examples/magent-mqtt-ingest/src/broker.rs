//! Tiny in-process MQTT 3.1.1 broker — handles CONNECT/CONNACK,
//! SUBSCRIBE/SUBACK, PUBLISH fan-out, and DISCONNECT for QoS 0
//! only. Single tokio task per connection, in-memory topic table.
//!
//! Originally lifted from `examples/mqtt-roundtrip/src/broker.rs`
//! so this example stays self-contained.

use crate::protocol::{decode_packet, encode_packet, Packet};
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

#[derive(Default, Clone)]
pub struct BrokerState {
    subs: Arc<Mutex<HashMap<String, Vec<Sender>>>>,
}

#[derive(Clone)]
pub struct Sender {
    inner: Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
}

impl Sender {
    pub async fn send(&self, p: &Packet) -> std::io::Result<()> {
        let mut buf = encode_packet(p);
        let mut g = self.inner.lock().await;
        g.write_all_buf(&mut buf).await
    }
}

impl BrokerState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a fresh listener on a kernel-assigned port. Returns
    /// the local address and a `JoinHandle` for the accept loop.
    pub async fn bind(
        self,
    ) -> std::io::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let me = self.clone();
        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let me2 = me.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, me2).await {
                                eprintln!("[broker] connection error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("[broker] accept error: {}", e);
                        break;
                    }
                }
            }
        });
        Ok((addr, handle))
    }
}

async fn handle_connection(
    stream: TcpStream,
    state: BrokerState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (read_half, write_half) = stream.into_split();
    let writer = Arc::new(Mutex::new(write_half));
    let sender = Sender { inner: writer.clone() };

    let mut buf = Vec::with_capacity(4096);
    let mut read = read_half;
    let mut client_id = String::new();

    loop {
        let mut tmp = [0u8; 2048];
        let n = match read.read(&mut tmp).await {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) => return Err(Box::new(e)),
        };
        buf.extend_from_slice(&tmp[..n]);

        while !buf.is_empty() {
            match decode_packet(&buf) {
                Ok((packet, consumed)) => {
                    buf.drain(..consumed);
                    match packet {
                        Packet::Connect { client_id: id } => {
                            client_id = id;
                            sender.send(&Packet::Connack).await?;
                        }
                        Packet::Subscribe { topic } => {
                            let mut g = state.subs.lock().await;
                            g.entry(topic.clone()).or_default().push(sender.clone());
                            sender.send(&Packet::Suback).await?;
                        }
                        Packet::Publish { topic, payload } => {
                            let g = state.subs.lock().await;
                            if let Some(subs) = g.get(&topic) {
                                let p = Packet::Publish {
                                    topic: topic.clone(),
                                    payload,
                                };
                                for s in subs {
                                    if let Err(e) = s.send(&p).await {
                                        eprintln!(
                                            "[broker] fan-out write failed ({}): {}",
                                            client_id, e
                                        );
                                    }
                                }
                            }
                        }
                        Packet::Disconnect => return Ok(()),
                        Packet::Connack | Packet::Suback => {}
                    }
                }
                Err(crate::protocol::CodecError::UnexpectedEof)
                | Err(crate::protocol::CodecError::Truncated) => break,
                Err(e) => return Err(Box::new(e)),
            }
        }
    }
}

impl BrokerState {
    #[allow(dead_code)]
    pub async fn publish(&self, topic: &str, payload: Bytes) {
        let p = Packet::Publish {
            topic: topic.to_string(),
            payload,
        };
        let g = self.subs.lock().await;
        if let Some(subs) = g.get(topic) {
            for s in subs {
                let _ = s.send(&p).await;
            }
        }
    }
}