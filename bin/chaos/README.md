# chaos

The CLI entry point. Dispatches to the appropriate subcommand, handles operator
login and configuration, and bootstraps the kernel for interactive or headless
sessions.

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
