# magent-mqtt-ingest

End-to-end demonstration of an mAgent ingress pipeline driven by a
real MQTT 3.1.1 subscriber.

## What it proves

- `magent-core` stays **free of `tokio`** / `rumqttc` — only the
  in-memory `MqttAdapter` + `push_frame` API lives there.
- A host-side integration can drop `rumqttc::AsyncClient` in via
  this `examples/` crate and wire the received `Publish` payloads
  into the chip-agnostic [`IngressGateway`].
- The full signing round-trip works over a real TCP+MQTT wire:

```text
broker ─► rumqttc::AsyncClient (tokio task)
              │   push_frame(&payload)
              ▼
         MqttAdapter ─► IngressGateway::ingest()
                          │   IngressMode::Signed
                          ▼
                       SignedMessage (JSON)
                          │
                          ▼
                    web3::verify_signed_message()
```

## Run

```sh
cargo run -p magent-mqtt-ingest --release
```

Expected output (deterministic):

```
=== mAgent MQTT Ingest (real wire, in-process broker) ===

[broker] bound on 127.0.0.1:NNNNN
[identity] did = did:key:z6Mk…
[subscriber] connected + subscribed to magent/cmd
[publisher] sent payload
[subscriber] received 21 raw bytes from broker
[gateway] ingested 21 bytes; envelope len = 275B
[verify] ✅ envelope verifies against device DID

=== end-to-end MQTT ingest verified ===
```

## Adapting for production

Three knobs:

| Change | Where |
| --- | --- |
| Swap the in-process broker for an external `mqtt://broker:1883` | `broker::BrokerState::bind()` in `src/main.rs`; just delete the broker block and pass your own `SocketAddr` |
| Switch to TLS (`mqtts://`) | add `rumqttc`'s `["tls"]` feature + `set_transport(Transport::Tls(…))` |
| Use a real device identity | replace `Identity::from_secret_bytes(&[7u8; 32])` with a key loaded from NVS / TPM |

## Threading contract

`MqttAdapter` is `!Sync` by design (single-threaded inbox). When
fanning in from a network thread, wrap it in
`Arc<tokio::sync::Mutex<MqttAdapter>>` and keep the critical
section in `push_frame` short — never hold the lock across an
`await` on the network path. See `src/main.rs::spawn_subscriber`
for the canonical pattern.

[`IngressGateway`]: https://docs.rs/magent_core/latest/magent_core/ingress/struct.IngressGateway.html