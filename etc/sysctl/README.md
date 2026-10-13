# chaos-sysctl

Layered configuration kernel. Merges and validates the database settings
layer, the project `.mcp.json` layer, and CLI overrides; enforces
`requirements.toml` constraints; and tracks the asset folders that anchor
`rules/` and `agents/` files.
