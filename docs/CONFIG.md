# System Configuration (`magent config`)

`magent config` is the CLI subcommand for managing the **runtime
configuration file** — the JSON record that controls how the agent
runner talks to LLM backends, how the conversation is compressed, and
how CLI defaults are surfaced.

It is the plumbing counterpart to `magent set-prompt`:

* `set-prompt` → system prompt **content** (what the LLM is told to do).
* `config`     → runtime **plumbing** (which model, which URL, how
  aggressive the compression pipeline).

## Why a separate file?

Before this feature, every knob had to be supplied on the command line:

```sh
magent run --provider deepseek --model deepseek-chat \
           --temperature 0.3 --num-predict 512 \
           --max-messages 32 --tool-max-chars 800 \
           "Read temperature"
```

That's tedious for daily use and impossible to audit. `magent config`
gives the same knobs a stable, version-controllable home so:

* The defaults you actually use are visible in one file.
* `git diff` shows every change since the last commit.
* Different projects can ship a checked-in config and users can
  override just the keys they care about.

## Layering

Effective configuration is computed by overlaying four layers in
this order (later wins):

1. **Built-in defaults** baked into the binary.
2. **`~/.config/magent/magent.json`** — this file.
3. **Environment variables** (`OLLAMA_HOST`, `DEEPSEEK_HOST`, …).
4. **CLI flags** (`--provider`, `--model`, `--temperature`, …).

Layer 2 is what this module manages. The other layers live in
`runner.rs` and are not duplicated here.

## Storage

| Source                                 | Path                                      |
| -------------------------------------- | ----------------------------------------- |
| `$MAGENT_CONFIG_FILE` (explicit)       | the value of the env var                  |
| `$MAGENT_CONFIG_DIR`                   | `$MAGENT_CONFIG_DIR/magent.json`          |
| `$XDG_CONFIG_HOME/magent/magent.json`  | XDG-compliant per-user path               |
| `$HOME/.config/magent/magent.json`     | macOS / Linux default                     |

If neither an explicit env var nor a writable home is available,
`config init` fails with a clear error rather than silently writing
to `/tmp`.

## Subcommands

```text
magent config init                 Create the config file at the canonical path
magent config where                Print the resolved config file path
magent config show                 Print the full JSON record (pretty)
magent config list                 Flatten every key/value pair
magent config get <KEY>            Read a single key (e.g. provider.ollama.url)
magent config set <KEY> <VALUE>    Write a single key
magent config reset [--yes]        Delete the config file (refuses without --yes)
magent config validate             Re-load and verify every field (CI-friendly exit codes)
magent config format               Re-serialise the file with canonical key order
```

### Example session

```sh
# Initialise with built-in defaults.
magent config init

# Read a single value.
magent config get sampling.temperature
# 0.3

# Change it.
magent config set sampling.temperature 0.7

# Audit every value as a flat list.
magent config list
# KEY                                      VALUE
# ----------------------------------------------------------------------
# schema_version                           1
# provider.default                         ollama
# provider.ollama.url                      http://localhost:11434
# provider.ollama.model                    llama3.2
# ...

# Wipe everything (refuses without --yes).
magent config reset
# error: refusing to reset without `--yes` (this deletes the config file)
magent config reset --yes
```

## On-disk JSON shape

```json
{
  "schema_version": 1,
  "provider": {
    "default": "ollama",
    "ollama": {
      "url": "http://localhost:11434",
      "model": "llama3.2",
      "api_key_env": "OLLAMA_API_KEY"
    },
    "deepseek": {
      "url": "https://api.deepseek.com/v1",
      "model": "deepseek-chat",
      "api_key_env": "DEEPSEEK_API_KEY"
    }
  },
  "sampling": {
    "temperature": 0.3,
    "num_predict": 512,
    "top_p": 1.0,
    "top_k": 40
  },
  "runner": {
    "max_iterations": 10,
    "max_tool_calls": 8,
    "probe_ollama_on_run": false
  },
  "compression": {
    "max_messages": 32,
    "tool_content_max_chars": 800
  },
  "io": {
    "no_color": false,
    "quiet_default": false,
    "json_default": false
  },
  "metadata": {
    "description": "...",
    "author": "...",
    "tags": ["..."]
  },
  "created_at": 1754716800,
  "updated_at": 1754720400
}
```

## Keys

### `provider.*`

| Key                                | Type   | Default                     |
| ---------------------------------- | ------ | --------------------------- |
| `provider.default`                 | string | `"ollama"`                  |
| `provider.ollama.url`              | string | `http://localhost:11434`    |
| `provider.ollama.model`            | string | `llama3.2`                  |
| `provider.ollama.api_key_env`      | string | `"OLLAMA_API_KEY"`          |
| `provider.deepseek.url`            | string | `https://api.deepseek.com/v1` |
| `provider.deepseek.model`          | string | `deepseek-chat`             |
| `provider.deepseek.api_key_env`    | string | `"DEEPSEEK_API_KEY"`        |

### `sampling.*`

| Key                         | Type    | Default |
| --------------------------- | ------- | ------- |
| `sampling.temperature`      | float   | `0.3`   |
| `sampling.num_predict`      | integer | `512`   |
| `sampling.top_p`            | float   | `1.0`   |
| `sampling.top_k`            | integer | `40`    |

### `runner.*`

| Key                             | Type    | Default |
| ------------------------------- | ------- | ------- |
| `runner.max_iterations`         | integer | `10`    |
| `runner.max_tool_calls`         | integer | `8`     |
| `runner.probe_ollama_on_run`    | boolean | `false` |

### `compression.*`

| Key                                       | Type    | Default |
| ----------------------------------------- | ------- | ------- |
| `compression.max_messages`                | integer | `32`    |
| `compression.tool_content_max_chars`      | integer | `800`   |

### `io.*`

| Key                       | Type    | Default |
| ------------------------- | ------- | ------- |
| `io.no_color`             | boolean | `false` |
| `io.quiet_default`        | boolean | `false` |
| `io.json_default`         | boolean | `false` |

### `metadata.*`

| Key                     | Type           | Default | Notes |
| ----------------------- | -------------- | ------- | ----- |
| `metadata.description`  | string \| null | `null`  | Free-form human description. Validated for length ≤ 1024 and no control characters. |
| `metadata.author`       | string \| null | `null`  | Owner / author name. Validated for length ≤ 256 and no control characters. |
| `metadata.tags`         | array\<string\>| `[]`    | Free-form tags. Each tag must be non-empty, ≤ 64 chars, no leading/trailing whitespace. Total count ≤ 32. |

Setting `metadata.tags` accepts four forms:

* A JSON array of strings: `magent config set metadata.tags '["a","b"]'`
* A comma-separated string: `magent config set metadata.tags "a,b"`.
* An empty array (`[]`) clears the list.
* `null` (also clears).

Setting `metadata.description` or `metadata.author` accepts three forms:

* A non-empty string: `magent config set metadata.author "alice"`.
* An empty string (`""`) clears.
* `null` (also clears).

All three fields reject whitespace-only strings and control characters
at write time.

## Secrets

API keys are **never** written to the config file. The
`provider.{ollama,deepseek}.api_key_env` field stores the **name** of
the env var that holds the key (e.g. `DEEPSEEK_API_KEY`); the actual
secret lives in your shell environment.

This is deliberate:

* `git diff` on the config file is safe to share.
* The config file is world-readable on shared hosts without leaking
  credentials.
* Rotating a key is a shell-environment change, not a config-file
  edit.

If you need to point the runner at a different env var, just update
`api_key_env`:

```sh
magent config set provider.deepseek.api_key_env MY_DEEPSEEK_KEY
```

## CLI vs config precedence

CLI flags always win. The config file is a *default* provider — any
explicit flag on the command line overrides the corresponding config
value. This means:

```sh
# Use the deepseek-chat model defined in config …
magent run --provider deepseek "Read temperature"

# … but this command overrides to deepseek-coder.
magent run --provider deepseek --model deepseek-coder "Read temperature"
```

## Audit & version-control

Because the config is a small JSON file in a known location, putting
it under git is straightforward:

```sh
git init ~/.config/magent  # one-time
git -C ~/.config/magnet add magent.json
git -C ~/.config/magent commit -m "initial config"
```

Future `config set` operations produce a normal `git diff` so you can
review every change.

## Schemas

* **v1** (current) — the layout above. Hand-written files that omit
  the `metadata` block or any individual section deserialise
  successfully because every field has a `#[serde(default)]` rule.
* **v0** — implicit: hand-written files that omit `schema_version`
  default to `1`. Old magent binaries that don't know about
  `metadata` will still load the rest.

## Programmatic API

For library users, [`magent::config`] re-exports the store and the
subcommand glue:

```rust
use magent::config::{ConfigCmd, ConfigAction, ConfigRecord};

// Read the on-disk config (returns defaults if the file is missing).
let record: ConfigRecord = magent::config::load().unwrap();

// Look up a single value.
let model: serde_json::Value =
    magent::config::get(&record, "provider.ollama.model").unwrap();

// Set a single value and write back.
let updated = magent::config::set(
    record,
    "sampling.temperature",
    serde_json::json!(0.7),
).unwrap();
magent::config::save(updated).unwrap();

// Drive the subcommand glue (mirrors `magent config init` etc.).
let mut out = magent::output::Output::new(
    magent::output::OutputKind::Human,
    true,
);
ConfigCmd::new(&ConfigAction::Init).execute(&mut out).unwrap();
```