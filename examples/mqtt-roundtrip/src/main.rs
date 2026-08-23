//! MQTT 3.1.1 round-trip — a real wire-level test that
//! exercises `rumqttc 0.24` against an in-process broker.
//!
//! Run with: `cargo run -p mqtt-roundtrip --release`

mod broker;
mod protocol;

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== MQTT Round-Trip (real wire, in-process broker) ===\n");

    // 1. Broker binds a port.
    let addr = test_broker_binds_a_free_port().await?;

    // 2. Subscriber connects + subscribes; we capture its
    //    AsyncClient + a oneshot that fires on the first
    //    inbound Publish.
    let (sub_client, publish_rx, sub_task) =
        test_subscriber_connects_and_subscribes(addr).await?;

    // 3. Publisher sends a payload; the subscriber's
    //    eventloop surfaces it on `publish_rx`.
    test_publisher_round_trips_a_payload(addr, publish_rx).await?;

    // 4. Disconnect; abort the subscriber task.
    test_subscriber_clean_disconnect(sub_client, sub_task).await?;

    println!("\n=== All round-trip tests passed ===");
    Ok(())
}

/// 1. Stand up the broker; assert it bound a real port.
async fn test_broker_binds_a_free_port(
) -> Result<std::net::SocketAddr, Box<dyn std::error::Error>> {
    println!("Test: broker binds a free port");

    let state = broker::BrokerState::new();
    let (addr, _handle) = state.bind().await?;
    assert!(addr.port() != 0, "kernel must assign a non-zero port");
    println!("  ✅ bound on {}", addr);
    Ok(addr)
}

/// 2. Connect a rumqttc subscriber, subscribe to "test/hello",
///    drive the eventloop until we've seen CONNACK + SUBACK,
///    then hand control of the eventloop to a background task
///    that forwards the first inbound Publish through a
///    oneshot. Returns the subscriber's AsyncClient (cheap to
///    clone; internally Arc), the oneshot receiver, and the
///    background task's JoinHandle so test 4 can abort it.
async fn test_subscriber_connects_and_subscribes(
    addr: std::net::SocketAddr,
) -> Result<
    (
        Arc<rumqttc::AsyncClient>,
        oneshot::Receiver<Vec<u8>>,
        JoinHandle<()>,
    ),
    Box<dyn std::error::Error>,
> {
    println!("Test: subscriber connects + subscribes");

    let mut opts = rumqttc::MqttOptions::new("sub-1", addr.ip().to_string(), addr.port());
    opts.set_keep_alive(Duration::from_secs(5));

    let (client, mut eventloop) = rumqttc::AsyncClient::new(opts, 8);
    // Subscribe queues the request internally — the eventloop
    // task we spawn below will flush it.
    client
        .subscribe("test/hello", rumqttc::QoS::AtMostOnce)
        .await?;

    let (publish_tx, publish_rx) = oneshot::channel::<Vec<u8>>();

    // Drive the eventloop ourselves until we've seen both
    // CONNACK and SUBACK (proving the wire handshake worked),
    // then hand it off to a background task that forwards the
    // next inbound Publish.
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
            Ok(Ok(other)) => eprintln!("  [sub] event: {:?}", other),
            Ok(Err(e)) => return Err(format!("subscriber error during handshake: {}", e).into()),
            Err(_) => continue,
        }
        if saw_connack && saw_suback {
            break;
        }
    }
    assert!(saw_connack, "subscriber never received CONNACK");
    assert!(saw_suback, "subscriber never received SUBACK");
    println!("  ✅ connected + subscribed");

    // Hand the eventloop off to a background task: keep
    // polling and forward the first inbound Publish.
    let sub_task = tokio::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(rumqttc::Event::Incoming(rumqttc::Incoming::Publish(p))) => {
                    let _ = publish_tx.send(p.payload.to_vec());
                    // Keep the eventloop alive so the broker
                    // sees a healthy peer until test 4 sends
                    // DISCONNECT.
                    loop {
                        if eventloop.poll().await.is_err() {
                            return;
                        }
                    }
                }
                Ok(_) => continue,
                Err(_) => return,
            }
        }
    });

    Ok((Arc::new(client), publish_rx, sub_task))
}

/// 3. Spin up a separate publisher, send a message, and
///    assert the subscriber's eventloop surfaces it on the
///    oneshot.
async fn test_publisher_round_trips_a_payload(
    addr: std::net::SocketAddr,
    publish_rx: oneshot::Receiver<Vec<u8>>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Test: publisher publishes + subscriber receives");

    let mut opts = rumqttc::MqttOptions::new("pub-1", addr.ip().to_string(), addr.port());
    opts.set_keep_alive(Duration::from_secs(5));
    let (pub_client, mut pub_eventloop) = rumqttc::AsyncClient::new(opts, 4);

    // Drain the publisher's eventloop in the background so
    // the queued CONNECT + PUBLISH actually hit the wire.
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
            "test/hello",
            rumqttc::QoS::AtMostOnce,
            false,
            vec![42u8, 42, 42, 42],
        )
        .await?;
    drain.await.ok();

    // Wait up to 3 s for the subscriber to surface it.
    let payload = match timeout(Duration::from_secs(3), publish_rx).await {
        Ok(Ok(p)) => p,
        Ok(Err(_)) => return Err("subscriber oneshot dropped before delivering".into()),
        Err(_) => return Err("subscriber never received the publish within 3s".into()),
    };
    assert_eq!(payload, vec![42u8, 42, 42, 42], "payload round-trip mismatch");
    println!("  ✅ payload round-tripped: {:?}", payload);
    Ok(())
}

/// 4. Disconnect the subscriber; abort the background task.
async fn test_subscriber_clean_disconnect(
    sub_client: Arc<rumqttc::AsyncClient>,
    sub_task: JoinHandle<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Test: subscriber disconnect is acknowledged");

    sub_client.disconnect().await.ok();
    // Give the broker a moment to react.
    tokio::time::sleep(Duration::from_millis(150)).await;
    sub_task.abort();
    let _ = sub_task.await;
    println!("  ✅ clean disconnect");
    Ok(())
}