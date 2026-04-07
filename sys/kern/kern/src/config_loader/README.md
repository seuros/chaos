# `chaos-kern` config loader

This module is the canonical place to **load and describe Chaos configuration layers** (user config, CLI/session overrides, and system config) and to produce:

- An **effective merged** TOML config.
- **Per-key origins** metadata (which layer "wins" for a given key).
- **Per-layer versions** (stable fingerprints) used for optimistic concurrency / conflict detection.

## Public surface

Exported from `chaos_kern::config_loader`:

- `load_config_layers_state(chaos_home, cwd_opt, cli_overrides, overrides) -> ConfigLayerStack`
- `ConfigLayerStack`
  - `effective_config() -> toml::Value`
  - `origins() -> HashMap<String, ConfigLayerMetadata>`
  - `layers_high_to_low() -> Vec<ConfigLayer>`
  - `with_user_config(user_config) -> ConfigLayerStack`
- `ConfigLayerEntry` (one layer's `{name, config, version, disabled_reason}`; `name` carries source metadata)
- `LoaderOverrides` (test/override hooks, currently empty)
- `merge_toml_values(base, overlay)` (public helper used elsewhere)

## Layering model

Precedence is **top overrides bottom**:

1. **System** config (`/etc/chaos/config.toml`)
2. **User** config (`$CHAOS_HOME/config.toml`)
3. **Project** config (`.chaos/config.toml` in project tree)
4. **Session flags** (CLI overrides, applied as dotted-path TOML writes)

Layers with a `disabled_reason` are still surfaced for UI, but are ignored when
computing the effective config and origins metadata. This is what
`ConfigLayerStack::effective_config()` implements.

## Typical usage

Most callers want the effective config plus metadata:

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

Implementation is split by concern:

- `state.rs`: public types (`ConfigLayerEntry`, `ConfigLayerStack`) + merge/origins convenience methods.
- `overrides.rs`: CLI dotted-path overrides -> TOML "session flags" layer.
- `merge.rs`: recursive TOML merge.
- `fingerprint.rs`: stable per-layer hashing and per-key origins traversal.
