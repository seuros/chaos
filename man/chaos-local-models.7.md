+++
title = "chaos-local-models(7)"
summary = "Connect FreeChaOS to self-hosted OpenAI-compatible servers: vLLM, Ollama, llama.cpp, LocalAI, SGLang, DGX Spark."
+++

# chaos-local-models(7)

## NAME

chaos-local-models - use a self-hosted or LAN model server as a FreeChaOS provider

## DESCRIPTION

Use a local or LAN OpenAI-compatible server through a custom provider entry.
No API key is needed unless the server requires one.

## REQUIREMENTS

FreeChaOS needs three things from the server:

| Endpoint | Used for |
|----------|----------|
| `GET /v1/models` | The model catalog shown by `chaos models` and the picker. |
| `POST /v1/chat/completions` with `stream: true` | Turns, with `tools` and structured `tool_calls` deltas. |
| `POST /v1/responses` (optional) | Used automatically when present and `wire_api` is `auto`. |

Structured tool calls are the part most often missing. When the server
turns the model's tool call into plain text instead of a `tool_calls`
array, FreeChaOS cannot execute anything. Each backend has a switch for
this; see BACKENDS.

Verify the server before touching FreeChaOS:

```bash
curl http://<host>:<port>/v1/models
```

## ADDING THE PROVIDER

Provider entries are persisted user settings, not `config.toml` keys. Add
one with `chaos config set` and a JSON object:

```bash
chaos config set model_providers.local \
  '{"name":"Local server","base_url":"http://<host>:<port>/v1","wire_api":"chat_completions"}'
chaos config set model_provider '"local"'
chaos config set model '"<model id>"'
```

Then list the catalog and run a turn:

```bash
chaos --provider local models --refresh
chaos --provider local -m <model id>
```

Do not put provider entries in `config.toml`; that file holds bootstrap
settings only. `-c model_providers.<id>.base_url=...` on the command line
cannot create a new provider either; it only overrides fields of one that
already exists. Inspect or change fields later with:

```bash
chaos config get model_providers.local
chaos config set model_providers.local.base_url '"http://<new host>:<port>/v1"'
```

Pass `-m <model>` explicitly together with `--provider`. The id must match
an entry from `/v1/models`. Several entries can point at the same host with
different `name` or `wire_api` values.

Keyless providers do not appear in `/accounts`; that list only shows
providers with credentials to connect. Select a keyless provider with
`--provider`, with the `model_provider` setting, or from the `/model` picker.

## WIRE FORMAT

| `wire_api` | Behavior |
|------------|----------|
| `chat_completions` | Recommended for self-hosted servers. Supported by every backend listed below. |
| `auto` (default) | Probes `/v1/responses` first. A 404, 405, or 501 falls back to Chat Completions for the session. A 200 keeps Responses, even if the implementation is partial. |
| `responses` | Responses only, no probe. |

Set `wire_api = "chat_completions"` unless the server's Responses
implementation is known to handle streamed function calls. A server that
answers `/v1/responses` with a 500 does not fall back.

## AUTHENTICATION

Omit `env_key` for a keyless server. FreeChaOS sends no `Authorization`
header in that case. If the server enforces a key, add:

```bash
chaos config set model_providers.local.env_key '"LOCAL_API_KEY"'
export LOCAL_API_KEY=<the key>
```

Provider-specific key format checks do not apply to custom ids. Keep
keyless servers on a trusted LAN; none of the backends below is an
authentication gateway. Do not forward their ports on a router.

## MODEL METADATA

The catalog comes from `GET /v1/models`. Context length fields that some
servers add there (`max_model_len`, `context_length`) are not read for
custom providers, so an unlisted model receives fallback metadata. Mirror
the server limit explicitly:

```bash
chaos config set model_context_window 32768
```

Image input and reasoning effort are not advertised by these servers;
FreeChaOS treats custom models as text-only unless the catalog says
otherwise. Duplicate ids returned by the server are shown as-is.

## BACKENDS

Defaults and the structured tool-call switch for each server:

| Server | Default `base_url` | Structured tool calls | Key |
|--------|--------------------|-----------------------|-----|
| vLLM | `http://host:8000/v1` | `--enable-auto-tool-choice --tool-call-parser <parser>` | `--api-key` |
| Ollama | `http://host:11434/v1` | Built in for models whose template declares tools | none |
| llama.cpp `llama-server` | `http://host:8080/v1` | `--jinja` | `--api-key` |
| LocalAI | `http://host:8080/v1` | Per-model YAML (`function:` section) | `--api-key` |
| SGLang | `http://host:30000/v1` | `--tool-call-parser <parser>` | `--api-key` |
| NVIDIA NIM | `http://host:8000/v1` | Built in | optional |

### vLLM

```bash
vllm serve Qwen/Qwen3-4B \
  --host 0.0.0.0 --port 8000 \
  --served-model-name qwen3-4b \
  --max-model-len 32768 \
  --enable-auto-tool-choice --tool-call-parser hermes \
  --reasoning-parser qwen3
```

`--host 0.0.0.0` is required for LAN access; the default binds loopback.
`--served-model-name` sets the id FreeChaOS lists. Pick the parser for the
model family: `hermes` for Qwen, `llama3_json` for Llama 3.x, `mistral` for
Mistral. `--reasoning-parser` separates thinking from the answer. Both
`chat_completions` and `auto` work against current vLLM releases; older
releases had incomplete Responses streaming for non-Harmony models, so
prefer `chat_completions` on unknown versions.

### Ollama

```bash
OLLAMA_HOST=0.0.0.0 ollama serve
ollama pull qwen3
```

`OLLAMA_HOST` is needed for LAN access. Tool calls work for models whose
Modelfile template declares them; models without a tool template answer in
prose. Ollama's `/v1/responses` is absent on older releases, so `auto`
falls back cleanly.

### llama.cpp

```bash
llama-server -m model.gguf --host 0.0.0.0 --port 8080 --jinja -c 32768
```

Without `--jinja`, tool calls are emitted as text. `-c` sets the context
length; mirror it with `model_context_window`.

### LocalAI

Tool calling is configured per model in its YAML under `function:`. Some
backends stream tool calls as text while still reporting a `tool_calls`
finish reason; see TROUBLESHOOTING. Models unload with
`POST /backend/shutdown`.

### NVIDIA DGX Spark

The Spark runs vLLM, NIM, or Ollama in containers. Publish the
port on all interfaces and use the Spark's LAN address from the FreeChaOS
machine:

```bash
docker run --gpus all -p 8000:8000 vllm/vllm-openai:latest \
  --model Qwen/Qwen3-4B --served-model-name qwen3-4b \
  --enable-auto-tool-choice --tool-call-parser hermes
hostname -I            # on the Spark: LAN address to use in base_url
```

A two-Spark cluster serves one endpoint from the head node; use that
address. Everything else is identical to the single-host case.

## TROUBLESHOOTING

**Connection refused from another machine** — the server is bound to
loopback. Restart with the backend's bind-all option (`--host 0.0.0.0`,
`OLLAMA_HOST`) or publish the container port,
then check the firewall on the server host.

**"No credentials found for provider"** — the provider has `env_key` set but
the variable is empty, or FreeChaOS predates keyless Chat Completions
support. Remove `env_key` for a keyless server or export the variable.

**"System message must be at the beginning"** — update FreeChaOS for compatibility
with single-system-message chat templates such as Qwen3.6/3.8.

**Tool calls appear as text and the turn repeats** — the server returned
`finish_reason: "tool_calls"` without a structured `tool_calls` array.
Enable the backend's tool-call switch from the BACKENDS table.

**First request times out** — vLLM and SGLang compile graphs on the first
run of a model; llama.cpp and Ollama load weights on demand. Wait for the
server to answer `/v1/models` quickly, or raise `stream_idle_timeout_ms` on
the provider.

**Small models never call tools** — models under roughly 8B parameters may
answer from memory instead of calling `enable_tools`. Ask for the tool
explicitly or pick a larger model; the transport is not at fault.

**Zero token usage** — the server does not fill `usage` in streamed
responses. Context tracking still works from FreeChaOS's own counts.

**Only one GPU** — vLLM and SGLang reserve most of the GPU memory while
running. Stop other local inference servers first, or lower
`--gpu-memory-utilization`.

## SEE ALSO

- [chaos-providers.7](./chaos-providers.7.md)
- [chaos-support.7](./chaos-support.7.md)
- [chaos-storage.7](./chaos-storage.7.md)
- [chaos-reflex.7](./chaos-reflex.7.md)
