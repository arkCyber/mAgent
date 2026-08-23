//! End-to-end MQTT ingestion pipeline for mAgent.
//!
//! Demonstrates how to wire a **real** MQTT subscriber
//! ([`rumqttc::AsyncClient`]) into the chip-agnostic
//! [`IngressGateway`]. All the MQTT protocol work happens here in
//! this `examples/` crate, so `magent-core` itself never has to
//! depend on `tokio`.
//!
//! ## Pipeline
//!
//! ```text
//! broker (in-process) ──► rumqttc::AsyncClient::poll()
//!                                   │ (tokio task)
//!                                   ▼
//!                          MqttAdapter::push_frame()
//!                                   │
//!                                   ▼ Arc<Mutex<MqttAdapter>>
//!                          IngressGateway::ingest()  ◄─── main thread loop
//!                                   │
//!                                   ▼
//!                          IngressFrame { payload, envelope_json }
//!                                   │
//!                                   ▼
//!                          web3::verify_signed_message()
//! ```
//!
//! ## Run
//!
//! ```sh
//! cargo run -p magent-mqtt-ingest --release
//! ```
//!
//! Expected output: a single round-trip line confirming the
//! signed envelope verified with the device's public key. Failure
//! here indicates a regression in the gateway ↔ web3 binding.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, Mutex};
use tokio::time::timeout;

use magent_core::communication::mqtt::MqttAdapter;
use magent_core::ingress::{IngressGateway, IngressMode};
use magent_core::web3::{verify_signed_message, Identity};

/// Re-use the broker / protocol stack from `mqtt-roundtrip`. These
/// modules are private to that crate, so we ship a parallel copy
/// here that is intentionally byte-identical — the `mqtt-roundtrip`
/// tests are the regression net for the broker + codec.
mod broker;
mod protocol;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== mAgent MQTT Ingest (real wire, in-process broker) ===\n");

    // 1. Stand up the broker.
    let (addr, _broker_task) = broker::BrokerState::new().bind().await?;
    println!("[broker] bound on {}", addr);

    // 2. Build a device identity. In a real product this would be
    //    persisted to NVS / TPM and reloaded on every boot; here
    //    we just synthesise one from a fixed seed so the example
    //    is deterministic.
    let identity = Identity::from_secret_bytes(&[7u8; 32])
    .map_err(|e| format!("identity: {:?}", e))?;
    println!("[identity] did = {}", identity.did_key());

    // 3. Subscribe via rumqttc on the "magent/cmd" topic. The
    //    adapter sits behind an `Arc<Mutex<…>>` so the rumqttc
    //    task (tokio) and the gateway ingest loop (main) can
    //    both reach it without races.
    let adapter = Arc::new(Mutex::new(MqttAdapter::new("magent/cmd")));
    let (sub_client, _eventloop_task, payload_tx) =
        spawn_subscriber(addr, Arc::clone(&adapter)).await?;

    // 4. Spin up a publisher that pushes one raw payload onto
    //    the same topic. The subscriber receives the raw bytes
    //    via `MqttAdapter::push_frame`, and the gateway's
    //    `Signed` mode wraps them in an envelope signed by
    //    the device identity. We then re-verify that envelope
    //    via `web3::verify_signed_message`, which closes the
    //    loop without needing two distinct DIDs.
    publish_one_signed_payload(addr, &identity).await?;
    println!("[publisher] sent payload");

    // 5. Wait for the subscriber to surface the payload, then
    //    drain the adapter through the gateway.
    let raw_payload = match timeout(Duration::from_secs(3), payload_tx).await {
        Ok(Ok(p)) => p,
        Ok(Err(_)) => return Err("subscriber oneshot dropped before delivering".into()),
        Err(_) => return Err("subscriber never received the publish within 3s".into()),
    };
    println!(
        "[subscriber] received {} raw bytes from broker",
        raw_payload.len()
    );

    // The rumqttc subscriber already pushed the bytes into the
    // adapter; now we drain through a Signed-mode gateway. The
    // gateway needs a signer to emit the envelope — we use the
    // publisher's identity since this is a closed-loop test.
    let mut gw: IngressGateway<MqttAdapter> =
        IngressGateway::new(IngressMode::Signed);
    gw.set_signer(
        Identity::from_secret_bytes(&[7u8; 32])
        .map_err(|e| format!("signer: {:?}", e))?,
    );

    // The gateway holds the adapter by value, so we extract the
    // one inside the `Arc<Mutex<…>>` and hand it over. After
    // this point the Arc is no longer reachable — fine because
    // we're about to exit.
    let adapter = Arc::try_unwrap(adapter)
    .map_err(|_| "another owner still holds the adapter")?
    .into_inner();
    gw.register(adapter)?;

    let frame = gw.ingest()?.expect("frame expected");
    let envelope = frame
    .envelope_json
    .as_ref()
    .expect("signed mode must emit an envelope");
    println!(
        "[gateway] ingested {} bytes; envelope len = {}B",
        frame.payload.len(),
        envelope.len()
    );

    // 6. Verify the envelope round-trips through the public
    //    web3::verify_signed_message API. This is the same check
    //    the ingress_tests run on the host, just with a real
    //    network hop in between signing and verifying.
    let signed = magent_core::web3::SignedMessage::from_json(envelope)
    .map_err(|e| format!("envelope parse: {:?}", e))?;
    assert!(
        verify_signed_message(&signed, &raw_payload),
        "envelope must verify with the same payload that was ingested",
    );
    println!("[verify] ✅ envelope verifies against device DID");

    // 7. Clean disconnect. The subscriber task exited naturally
    //    after the first publish (we needed to release the
    //    `Arc<Mutex<MqttAdapter>>` so we could unwrap it), so
    //    there's nothing to abort here.
    sub_client.disconnect().await.ok();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let _ = _eventloop_task.await;

    println!("\n=== end-to-end MQTT ingest verified ===");
    Ok(())
}

/// Spawn the rumqttc subscriber on its own tokio task and forward
/// the first inbound `Publish` payload through a `oneshot`.
async fn spawn_subscriber(
    addr: std::net::SocketAddr,
    adapter: Arc<Mutex<MqttAdapter>>,
) -> Result<
    (
        Arc<rumqttc::AsyncClient>,
        tokio::task::JoinHandle<()>,
        oneshot::Receiver<Vec<u8>>,
    ),
    Box<dyn std::error::Error>,
> {
    let mut opts = rumqttc::MqttOptions::new("sub-1", addr.ip().to_string(), addr.port());
    opts.set_keep_alive(Duration::from_secs(5));
    let (client, mut eventloop) = rumqttc::AsyncClient::new(opts, 8);
    client.subscribe("magent/cmd", rumqttc::QoS::AtMostOnce).await?;

    // Drive the eventloop until we've seen CONNACK + SUBACK.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut saw_connack = false;
    let mut saw_suback = false;
    while Instant::now() < deadline {
        match timeout(Duration::from_millis(250), eventloop.poll()).await {
            Ok(Ok(rumqttc::Event::Incoming(rumqttc::Incoming::ConnAck(_)))) => {
                saw_connack = true;
            }
            Ok(Ok(rumqttc::Event::Incoming(rumqttc::Incoming::SubAck(_)))) => {
                saw_suback = true;
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(format!("subscriber error: {}", e).into()),
            Err(_) => continue,
        }
        if saw_connack && saw_suback {
            break;
        }
    }
    assert!(saw_connack, "subscriber never received CONNACK");
    assert!(saw_suback, "subscriber never received SUBACK");
    println!("[subscriber] connected + subscribed to magent/cmd");

    let (publish_tx, publish_rx) = oneshot::channel::<Vec<u8>>();
    let adapter_for_task = Arc::clone(&adapter);
    let sub_task = tokio::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(rumqttc::Event::Incoming(rumqttc::Incoming::Publish(p))) => {
                    // Critical section: copy the payload bytes
                    // and release the mutex BEFORE doing anything
                    // else. Holding the lock across an await on
                    // a slow path is a classic deadlock recipe.
                    let bytes = p.payload.to_vec();
                    {
                        let mut a = adapter_for_task.lock().await;
                        if let Err(e) = a.push_frame(&bytes) {
                            eprintln!("[subscriber] push_frame failed: {:?}", e);
                            return;
                        }
                    }
                    // Hand the raw payload to the main thread so
                    // it can verify the envelope, then exit. The
                    // main thread then takes sole ownership of
                    // `Arc<Mutex<MqttAdapter>>` and unwraps it
                    // before feeding the gateway.
                    let _ = publish_tx.send(bytes);
                    return;
                }
                Ok(_) => continue,
                Err(_) => return,
            }
        }
    });

    Ok((Arc::new(client), sub_task, publish_rx))
}

/// Publish a single payload through a `rumqttc` publisher.
/// Returns when the broker has acknowledged the SUBSCRIBE; the
/// caller is expected to wait on a oneshot for delivery.
///
/// Note: the publisher here sends **raw plaintext**. In
/// production the publisher would typically sign its own
/// payload and send the resulting envelope; the receiving
/// gateway would then verify the *publisher's* signature
/// (using `web3::verify_signed_message` directly) before
/// optionally re-signing on its own DID. We keep this
/// example as a single-DID round trip so the assertion stays
/// deterministic across runs.
async fn publish_one_signed_payload(
    addr: std::net::SocketAddr,
    _identity: &Identity,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut opts = rumqttc::MqttOptions::new("pub-1", addr.ip().to_string(), addr.port());
    opts.set_keep_alive(Duration::from_secs(5));
    let (pub_client, mut pub_eventloop) = rumqttc::AsyncClient::new(opts, 4);

    // Drain the publisher's eventloop in the background.
    let drain = tokio::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match timeout(Duration::from_millis(200), pub_eventloop.poll()).await {
                Ok(Ok(_)) => continue,
                _ => break,
            }
        }
    });

    pub_client
    .publish(
        "magent/cmd",
        rumqttc::QoS::AtMostOnce,
        false,
        b"{\"cmd\":\"hello-agent\"}".to_vec(),
    )
    .await?;
    drain.await.ok();
    pub_client.disconnect().await.ok();
    Ok(())
}