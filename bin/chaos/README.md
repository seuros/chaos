# chaos

The CLI entry point. Dispatches to the appropriate subcommand, handles operator
login and configuration, and bootstraps the kernel for interactive or headless
sessions.

The default `tui` Cargo feature includes the interactive console. Build with
`cargo build -p chaos-cli --bin chaos --no-default-features` for a renderer-free
binary supporting `serve`, `mcp serve`, `taskd`, and `exec`. See
[build and deployment](../../man/chaos-install.7.md#build-without-the-tui).

## Account management

Connect a provider subscription account with device authorization:

- ChatGPT:
  - `chaos --provider openai accounts --device-auth`
- xAI:
  - `chaos --provider xai accounts --device-auth`

The command prints a verification URL and one-time code, then stores the
resulting provider-scoped OAuth credentials in Chaos's normal credential
store (`auth.json` when file storage is configured). Existing API-key
connections remain unchanged unless this command is run for that provider.

xAI subscription auth uses xAI's public Grok CLI OAuth client and device-code
endpoints. Authenticated model requests are sent to the Grok CLI subscription
proxy; API-key requests continue to use `https://api.x.ai/v1`.

Use the CLI to disconnect stored provider credentials:

- disconnect the active provider:
  - `chaos accounts disconnect`
- disconnect all stored providers:
  - `chaos accounts disconnect --all`
  - `chaos logout`

The interactive TUI exposes `/accounts` for connecting and managing providers,
but it does not expose `/logout`. Disconnect accounts from the CLI instead.

API-key saves in `/accounts` and the CLI share kernel validation. Keys with the
wrong format for a known provider are rejected before writing credentials; an
existing saved account remains unchanged. Validation does not detect or switch
the selected provider. Format rules are built into the harness, not configurable.
Custom provider IDs accept their own key formats, subject to nonempty,
printable-ASCII checks.

For Kimi models, choose **Moonshot AI** (pay-per-token API) or **Moonshot AI
Coding** (Kimi Code subscription) in `/accounts`, then paste the corresponding
API key. Keys are stored separately under `moonshotai` and `moonshotai-coding`;
you do not need to export environment variables when connecting this way.
