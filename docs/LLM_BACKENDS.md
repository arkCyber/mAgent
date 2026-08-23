# mAgent LLM Backends

The agent runner talks to "the LLM" through a small trait so we can
swap providers without touching the ReAct loop. This document covers
the two implementations we ship today and how to wire them up.

## Trait: `LlmBackend`

```rust
pub trait LlmBackend: Send + std::any::Any {
    fn check_connection(&self) -> bool;
    fn chat_with_messages(
        &mut self,
        messages: &[Message],
        sampling: SamplingParams,
    ) -> Result<String, String>;
    fn provider(&self) -> LlmProvider;
    fn model(&self) -> &str;
    fn base_url(&self) -> &str;
}
```

The runner holds an `Option<Box<dyn LlmBackend>>` and flips it to
`Some(...)` automatically the first time `check_connection()` succeeds.

`RealAgentRunner` exposes:

| Method | Purpose |
|---|---|
| `backend_mut()` | Mutable access to the wired backend, if any |
| `backend_provider()` | `Some(LlmProvider::Ollama / DeepSeek)` |
| `set_backend(B)` | Swap in a different backend (resets the probe flag) |
| `force_enable_backend()` | Skip the auto-probe and turn the backend on. No-op if no backend is wired up. |
| `force_disable_backend()` | Turn the backend off (will re-probe on next `run()` if `probe_ollama_on_run` is true). |
| `using_backend()` | `true` if the probe succeeded |
| `using_ollama()` | Kept for backwards compatibility; `true` only if the backend is Ollama |

---

## Ollama (default)

Local-first. Talks to anything speaking the Ollama HTTP API on port
11434. No API key required.

```rust
use magent_core::agent_runner::OllamaClient;
let client = OllamaClient::new("http://localhost:11434", "llama3.2");
runner.set_backend(client);
```

Or via the CLI:

```sh
magent run "Read the temperature"
# → uses http://localhost:11434 with model `llama3.2`
magent run --ollama http://gpu:11434 --model qwen2.5:7b "..."
```

Pull a model first:

```sh
ollama pull llama3.2
```

### Environment variables

| Variable | Default | Used for |
|---|---|---|
| `OLLAMA_HOST` | `http://localhost:11434` | Ollama base URL (`doctor` only) |
| `OLLAMA_MODEL` | `llama3.2` | Default model name (`doctor` only) |
| `OLLAMA_API_KEY` | _unset_ | Last-resort fallback for `--api-key` |

The `run` subcommand does **not** read `OLLAMA_HOST` /
`OLLAMA_MODEL` because its defaults live on `RunOptions`. To override
from the environment, prefer setting the same defaults via CLI flags,
or patch `RunOptions::default()` in `cli/src/cli.rs`.

---

## DeepSeek

Hosted API at `https://api.deepseek.com/v1`. OpenAI-compatible.
**Requires an API key.**

```rust
use magent_core::agent_runner::DeepSeekClient;
let client = DeepSeekClient::new("sk-...");
runner.set_backend(client);
```

Or via the CLI:

```sh
magent run --provider deepseek --api-key sk-... "..."
# or via env var (recommended; the key isn't echoed in shell history):
DEEPSEEK_API_KEY=sk-... magent run --provider deepseek "..."
```

### Available models

| Name | Notes |
|---|---|
| `deepseek-chat` | Default; general-purpose chat. Same model as `deepseek-v3`. |
| `deepseek-reasoner` | Reasoning mode; slower + pricier but more accurate on math/code. |
| `deepseek-coder` | Code-specialised variant (third-party fine-tune, not DeepSeek's hosted API). |

To pick a model:

```sh
magent run --provider deepseek --model deepseek-reasoner "Solve ..."
```

### Environment variables

| Variable | Default | Used for |
|---|---|---|
| `DEEPSEEK_API_KEY` | _unset_ | Primary key source for `--provider deepseek` |
| `DEEPSEEK_HOST` | `https://api.deepseek.com/v1` | Override the base URL |
| `MAGENT_PROVIDER` | `ollama` | Provider for `doctor` when not given on the command line |

### Key resolution order

1. `--api-key <KEY>` (highest priority)
2. `DEEPSEEK_API_KEY` env var
3. `OLLAMA_API_KEY` env var (symmetry / fallback)
4. None → fall back to simulated reasoning

### Wire format

`POST {base_url}/chat/completions`
`Authorization: Bearer <api_key>`
`Content-Type: application/json`

```json
{
  "model": "deepseek-chat",
  "messages": [
    {"role": "system", "content": "..."},
    {"role": "user",   "content": "..."}
  ],
  "temperature": 0.3,
  "max_tokens": 512,
  "stream": false
}
```

Response:

```json
{
  "choices": [
    {"message": {"role": "assistant", "content": "..."}}
  ]
}
```

---

## Doctor command

`magent doctor` performs sanity checks for whichever provider is
configured. With `--provider deepseek`, it verifies:

1. An API key is present (CLI, `DEEPSEEK_API_KEY`, or `OLLAMA_API_KEY`)
2. `GET {base_url}/models` is reachable with that key

With the default Ollama provider, it verifies:

1. Ollama is reachable
2. The configured model appears in `/api/tags`
3. The tool backend (`SimulatorExecutor`) responds to a smoke test

In `--json` mode, the result is a structured envelope:

```json
{
  "ok": true,
  "checks": {
    "provider":      {"ok": true, "skipped": false, "name": "deepseek"},
    "ollama":        {"ok": false, "skipped": true,  "url": "http://localhost:11434"},
    "model":         {"ok": false, "skipped": true,  "name": "deepseek-chat"},
    "tool_backend":  {"ok": true}
  }
}
```

---

## Adding a new provider

1. Implement `LlmBackend` for your client struct.
2. Provide a constructor that takes whatever config the provider
   needs (API key, base URL, model name).
3. Implement `provider() -> LlmProvider` (extend the enum if it's a
   brand-new provider).
4. Add CLI plumbing in `cli/src/runner.rs` to swap your client in
   when `--provider <NAME>` is passed.
5. Add CLI plumbing in `cli/src/doctor.rs::check_provider_chain` for
   the sanity check.
6. Add unit tests that exercise the JSON body / response parsing
   without hitting the network (use `write_chat_body_for_test` as a
   template).