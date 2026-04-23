# chaos

The CLI entry point. Dispatches to the appropriate subcommand, handles operator
login and configuration, and bootstraps the kernel for interactive or headless
sessions.

## Account management

Use the CLI to disconnect stored provider credentials:

- disconnect the active provider:
  - `chaos accounts disconnect`
- disconnect all stored providers:
  - `chaos accounts disconnect --all`
  - `chaos logout`

The interactive TUI exposes `/accounts` for connecting and managing providers,
but it does not expose `/logout`. Disconnect accounts from the CLI instead.
