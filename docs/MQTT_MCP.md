# MQTT MCP Server

`magent-mqtt-mcp` is the MQTT counterpart to [`email-mcp`](EMAIL_MCP.md).
It speaks **MQTT 3.1.1 over plain TCP** and exposes the broker surface to
mAgent agents through the [Model Context Protocol](https://modelcontextprotocol.io/).

## Tools

| Tool              | Purpose                                                  |
|-------------------|----------------------------------------------------------|
| `publish_event`   | Publish a UTF-8 payload to a topic                       |
| `subscribe_topic` | Register a topic filter                                  |
| `broker_status`   | Diagnostics: broker endpoint + connection state          |

## Configuration

Configuration is read from `~/.config/magent/mqtt-mcp.toml` and overridden
by environment variables:

| Env var                  | TOML key              | Default            |
|--------------------------|-----------------------|--------------------|
| `MQTT_BROKER_HOST`       | `broker_host`         | `localhost`        |
| `MQTT_BROKER_PORT`       | `broker_port`         | `1883`             |
| `MQTT_CLIENT_ID`         | `client_id`           | `magent-cli`       |
| `MQTT_KEEP_ALIVE_SECS`   | `keep_alive_secs`     | `30`               |
| `MQTT_DEFAULT_TOPIC`     | `default_topic`       | `magent/events`    |
| `MQTT_USERNAME`          | `username`            | (empty)            |
| `MQTT_PASSWORD`          | `password`            | (empty)            |
| `MQTT_QOS`               | `qos_default`         | `1`                |

## Wire format

JSON-RPC 2.0 over newline-delimited stdio. Each line on stdin is one
JSON-RPC request; each line on stdout is one JSON-RPC response (or
notification). This is the standard MCP stdio transport.

Example session:

```text
→ {"jsonrpc":"2.0","id":1,"method":"initialize"}
← {"id":1,"jsonrpc":"2.0","result":{"protocolVersion":"2024-11-05",
    "serverInfo":{"name":"magent-mqtt-mcp","version":"0.1.0"},
    "capabilities":{"tools":{"listChanged":false}}}}

→ {"jsonrpc":"2.0","id":2,"method":"tools/list"}
← {"id":2,"jsonrpc":"2.0","result":{"tools":[
    {"name":"publish_event", ...},
    {"name":"subscribe_topic", ...},
    {"name":"broker_status", ...}]}}

→ {"jsonrpc":"2.0","id":3,"method":"tools/call",
   "params":{"name":"publish_event",
             "arguments":{"topic":"magent/events",
                          "payload_json":{"heart_rate":72}}}}
← {"id":3,"jsonrpc":"2.0","result":{
    "content":[{"type":"text","text":
      "{\"result\":{\"bytes\":16,\"published\":true,\"qos\":1,
                    \"retain\":false,\"topic\":\"magent/events\"},
                 \"broker\":\"localhost:1883\"}"}],
    "isError":false}}
```

## Running

```bash
# Default — talks to localhost:1883
./magent-mqtt-mcp

# Custom broker
MQTT_BROKER_HOST=10.0.0.42 MQTT_BROKER_PORT=1883 ./magent-mqtt-mcp

# Diagnostics
./magent-mqtt-mcp --show-config
```

## Limitations

- **TLS is not enabled by default** — the embedded nRF52 gateway
  doesn't have the ROM budget for it. An opt-in `tls` feature can
  be added behind a Cargo feature flag.
- **Subscribe is one-shot** — the tool acknowledges the SUBSCRIBE
  packet but doesn't stream incoming PUBLISH frames back through
  the JSON-RPC channel. Use `mosquitto_sub` alongside if you need
  to capture the actual messages.
- **Payload cap is 1 MiB** by default, matching `lettre`'s SMTP
  limit so email and MQTT can hold each other's payloads without
  surprises.
