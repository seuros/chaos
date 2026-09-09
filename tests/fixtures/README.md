# Shared test fixtures

Add `chaos-test-fixtures = { workspace = true }` under `[dev-dependencies]`
and use `chaos_test_fixtures::TEST_MODEL` for generic model identifiers,
including serialized mock responses.

`TEST_MODEL` is synthetic (`gpt-6-astra`), not a provider default or catalog entry.
Tests for particular model capabilities or differences between models should
retain their specific identifiers.

Keep catalog/profile matrices, prefix-matching and model-switching cases,
recorded provider error messages, and fixed-width UI snapshot examples explicit.
Do not replace `chatgpt-*` protocol headers or production model rules: they are
not generic test model identifiers.
