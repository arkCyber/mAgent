# Stored System Prompts (`magent set-prompt`)

`magent set-prompt` is the CLI subcommand for managing system prompts as
version-controllable JSON files instead of loose `.txt` blobs. Each prompt
is a small JSON record you can `git diff`, audit, and reuse across `run`
invocations.

## Why

Before this feature, the only way to swap a system prompt was
`magent run --prompt /path/to/foo.txt`. That works but:

* the prompt file is opaque to tools — `grep`, `jq`, `git log` all see it
  as one big string blob;
* there's no place to record the *author*, *description*, or *tags*;
* there's no audit trail (when was it first written? last updated?);
* sharing prompts between team members means emailing `.txt` files.

`magent set-prompt` replaces all of that with a one-record-per-file
JSON store.

## Storage layout

By default prompts live under the user's XDG data directory:

| Platform | Resolved path                                       |
| -------- | --------------------------------------------------- |
| Linux    | `$XDG_DATA_HOME/magent/prompts/<name>.json`         |
| macOS    | `$HOME/.local/share/magent/prompts/<name>.json`     |
| Override | `$MAGENT_PROMPTS_DIR`                               |

`MAGENT_PROMPTS_DIR` is the explicit override — useful for CI runners,
containers, and tests where `$XDG_DATA_HOME` may be unset.

## Subcommands

```text
magent set-prompt set    <NAME> --prompt <TEXT|FILE> [--provider …] [--model …]
                       [--description …] [--author …] [--tag <T>]…
magent set-prompt show   <NAME>
magent set-prompt list
magent set-prompt delete <NAME>
magent set-prompt export <NAME> > out.txt
```

Every sub-action prints either a pretty message (human mode) or a JSON
envelope (`--json` mode), mirroring the `magent run` output.

### `set`

```sh
magent set-prompt set health_coach \
    --prompt /etc/magent/prompts/health.md \
    --provider ollama \
    --model llama3.2 \
    --description "Embedded nRF52 health agent." \
    --author "you@example.com" \
    --tag wearable \
    --tag nrf52
```

The `--prompt` value is treated as a path *if it points to an existing
file*, otherwise as the literal prompt text. This lets you write the
prompt in your favourite editor and save it as JSON.

`set` is idempotent — re-running it overwrites the file but preserves
`created_at` (so audit logs see when it was first written) and refreshes
`updated_at`.

### `show` / `list` / `delete` / `export`

All four follow the standard `git`-style pattern:

* `show <NAME>` — print the full JSON record (pretty).
* `list` — print a table of every prompt (`NAME`, `PROVIDER`, `MODEL`, `TAGS`).
* `delete <NAME>` — remove the file (no-op if it's already gone).
* `export <NAME>` — print *just the prompt text* (pipeable into scripts).

### `template <NAME>`

Render a stored prompt with `{{KEY}}` placeholders substituted.
Variables come from two sources, merged in `--var` → `--vars-from` order
(so `--var` wins on conflict):

* `--var KEY=VALUE` — repeatable; later wins.
* `--vars-from <PATH>` — a JSON object whose keys are variable names
  and whose values are strings, numbers, booleans, or `null`.

```sh
magent set-prompt template greet --var name=Alice --var role=admin
magent set-prompt template greet --vars-from /tmp/vars.json
```

The rendered text is written to **stdout** (no decoration) so it pipes
cleanly into `magent run --prompt "$(...)"`. In JSON mode, the same
information is wrapped in an envelope.

Unfilled placeholders are *left as-is* in the output (so the user can
see what was missing) and ALSO surfaced via:

* A `warning: unfilled placeholders: …` line on stderr (Human mode).
* An `unfilled` array in the JSON envelope (JSON mode).

## On-disk JSON shape

Every `<name>.json` file is identical to what `show` would print:

```json
{
  "schema_version": 1,
  "name": "health_coach",
  "prompt": "You are mAgent, an embedded AI health coach...",
  "provider": "ollama",
  "model": "llama3.2",
  "metadata": {
    "description": "Embedded nRF52 health agent.",
    "author": "you@example.com",
    "tags": ["wearable", "nrf52"]
  },
  "created_at": 1754716800,
  "updated_at": 1754720400
}
```

* **`schema_version`** — bump this (and
  `magent::prompt::CURRENT_SCHEMA_VERSION`) if you ever change the JSON
  shape in a breaking way. Newer magent binaries refuse to load files
  with a higher `schema_version`.
* **`name`** — must be filesystem-safe (no `/`, no `..`).
* **`provider` / `model`** — empty means "use the provider default";
  non-empty wins over `magent run --provider` only when the user
  didn't explicitly override on the CLI.
* **`metadata.tags`** — always serialised as an array (possibly empty)
  so `grep` / `jq` queries work.
* **`created_at` / `updated_at`** — Unix seconds; `created_at` survives
  updates.

## Integration with `magent run`

`run` resolves the system prompt in this priority order:

1. `--prompt-name <NAME>` — load from the prompt store.
2. `--prompt <FILE>` — load a hand-written `.txt` file.
3. The built-in `HEALTH_SYSTEM_PROMPT` baked into the binary.

So a stored prompt wins over a `.txt` file but the user's
`--provider` / `--model` flags on the `run` command line still win
over the prompt's tags. (`prompt::resolve_for_run` only fills in
blanks.)

```sh
# Use the stored "health_coach" prompt; if it was tagged with
# --provider ollama and --model llama3.2, those become the defaults.
magent run --prompt-name health_coach "Read my heart rate"

# Force a different provider even though the prompt was tagged for ollama:
magent run --prompt-name health_coach --provider deepseek "Read my heart rate"
```

## Audit & version-control

Because each prompt is a small JSON file in a known directory, putting
the store under git is straightforward:

```sh
git init ~/.magent  # one-time
git -C ~/.magent add prompts/
git -C ~/.magent commit -m "add health_coach prompt"
```

Future `set` operations produce a normal `git diff`:

```diff
 {
   "schema_version": 1,
   "name": "health_coach",
-  "prompt": "You are a health coach.",
+  "prompt": "You are a health coach. Always respond in JSON.",
   "provider": "ollama",
   "model": "llama3.2",
   "metadata": {
-    "tags": ["wearable"]
+    "tags": ["wearable", "json-mode"]
   },
-  "created_at": 1754716800,
+  "created_at": 1754716800,
-  "updated_at": 1754716800
+  "updated_at": 1754800000
 }
```

## Programmatic API

For library users, [`magent::prompt`] re-exports the store and the
subcommand glue:

```rust
use magent::prompt::{SetPromptCmd, SetPromptAction, SetPromptSetOptions};

let action = SetPromptAction::Set(SetPromptSetOptions {
    name: "alpha".into(),
    prompt: "You are …".into(),
    provider: Some("ollama".into()),
    model: None,
    description: None,
    author: None,
    tags: vec!["test".into()],
});

let cmd = SetPromptCmd::new(&action);
let mut out = magent::output::Output::new(
    magent::output::OutputKind::Human,
    true,
);
cmd.execute(&mut out)?;
```

Resolution helpers:

```rust
use magent::prompt::resolve_for_run;

let resolved = resolve_for_run(&opts)?;
// resolved.text       → system prompt body
// resolved.provider   → Option<String> (Some only when the prompt
//                                     specifies a non-empty provider)
// resolved.model      → Option<String>
```

Template rendering:

```rust
use magent::prompt::{render_template, render_template_with_warnings};
use std::collections::BTreeMap;

let mut vars = BTreeMap::new();
vars.insert("name".to_string(), "Alice".to_string());

// Simple case: just the rendered text.
let out = render_template("Hello {{name}}.", &vars);

// Or get the unfilled-placeholder list too — useful for preview UIs.
let (rendered, unfilled) = render_template_with_warnings(
    "Hello {{name}}, you are {{role}}.",
    &vars,
);
// rendered = "Hello Alice, you are {{role}}."
// unfilled = ["role"]
```

## Schemas

* **v1** (current) — the layout above. Anything tagged `schema_version`
  ≤ 1 deserialises as `PromptRecord`.
* **v0** — implicit: hand-written files that omit `schema_version`
  default to `1` via `#[serde(default)]` on
  `PromptRecord::schema_version`. Old magent binaries that don't
  understand the field will still load the rest.

## Why no LLM summarisation step?

A common feature request is "automatically summarise the oldest
messages" — but we deliberately don't do it here. The compression
pipeline in `magent-core::conversation` already bounds the live
payload, and the JSON store is meant to be **audited by humans** with
`git diff`, not edited by an LLM. If you want summarisation, run
`magent set-prompt export <NAME>` through a separate summarisation
script and re-import with `set` — that way the audit chain stays
intact.