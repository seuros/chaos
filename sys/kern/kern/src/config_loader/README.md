# `chaos-kern` config loader

This module is the canonical place to **load and describe Chaos configuration layers** (database user settings, the project `.mcp.json` layer, and CLI/session overrides) and to produce:

- An **effective merged** config (`toml::Value` in memory; the database stores JSON).
- **Per-key origins** metadata (which layer "wins" for a given key).
- **Per-layer versions** (stable fingerprints) used for optimistic concurrency / conflict detection.
- The **asset folders** that anchor file-based `rules/` and `agents/`.

## Public surface

Exported from `chaos_kern::config_loader`:

- `load_config_layers_state(chaos_home, cwd_opt, cli_overrides, overrides) -> ConfigLayerStack`
- `ConfigLayerStack`
  - `effective_config() -> toml::Value`
  - `origins() -> HashMap<String, ConfigLayerMetadata>`
  - `layers_high_to_low() -> Vec<ConfigLayer>`
  - `asset_folders() -> &[AssetFolder]`
  - `with_database_settings(revision, settings) -> ConfigLayerStack`
- `ConfigLayerEntry` (one layer's `{name, config, version, disabled_reason}`; `name` carries source metadata)
- `AssetFolder { scope: Admin | Home | Project, dir, disabled_reason }`
- `LoaderOverrides` (test/override hooks, currently empty)
- `merge_toml_values(base, overlay)` (public helper used elsewhere)

## Layering model

Precedence is **top overrides bottom**:

1. **User database** settings (the runtime database; written by `chaos config set`)
2. **Project MCP** (`<project-root>/.mcp.json`, disabled when the project is untrusted)
3. **Session flags** (CLI overrides, applied as dotted-path writes)

Admin constraints from `/etc/chaos/requirements.toml` are applied on top of the
merged result and can only narrow it. Bootstrap (`storage_url`, `egress_url` in
`$CHAOS_HOME/config.toml`) is not a layer; the builder resolves it once and
passes it to `Config` directly.

Layers with a `disabled_reason` are still surfaced for UI, but are ignored when
computing the effective config and origins metadata.

## Asset folders

File-based assets are discovered through `asset_folders()`, lowest precedence first:

1. `/etc/chaos` (`Admin`): the only scope whose `rules/*.decrees` may grant.
2. Every `<dir>/.chaos` between the project root and `cwd` (`Project`): restrictions only, and
   carried with a `disabled_reason` when the project is not trusted.

The user's own rules (`chaos rules`) and roles (`chaos agents`) are database rows, merged as the
lowest user-controlled layer: `user_settings::rules::database_user_restrictions` for exec policy
and `ConfigOverrides::database_agent_roles` (filled by `ConfigBuilder::build`) for roles.

## Typical usage

```rust
use chaos_kern::config_loader::{load_config_layers_state, LoaderOverrides};
use chaos_realpath::AbsolutePathBuf;
use toml::Value as TomlValue;

let cli_overrides: Vec<(String, TomlValue)> = Vec::new();
let cwd = AbsolutePathBuf::current_dir()?;
let layers = load_config_layers_state(
    &chaos_home,
    Some(cwd),
    &cli_overrides,
    LoaderOverrides::default(),
).await?;

let effective = layers.effective_config();
let origins = layers.origins();
let layers_for_ui = layers.layers_high_to_low();
```

## Internal layout

Implementation is split by concern (shared code lives in `etc/sysctl`):

- `state.rs`: public types (`ConfigLayerEntry`, `ConfigLayerStack`) + merge/origins convenience methods.
- `assets.rs`: `AssetFolder` / `AssetScope`.
- `overrides.rs`: CLI dotted-path overrides -> "session flags" layer.
- `merge.rs`: recursive merge.
- `fingerprint.rs`: stable per-layer hashing and per-key origins traversal.
