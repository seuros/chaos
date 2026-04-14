# chaos-ration

Usage metering trait for AI providers. Defines the interface — vendors implement it.

Chaos doesn't list or bundle any providers. Provider crates depend on `chaos-ration` and implement `UsageProvider`. The ports tree distributes them.

```rust
impl UsageProvider for MyProvider {
    fn name(&self) -> &str { "my-provider" }
    async fn fetch_usage(&self) -> Result<Usage, RationError> { /* ... */ }
}
```
