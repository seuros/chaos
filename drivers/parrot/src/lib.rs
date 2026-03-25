//! Chaos Parrot — provider adapters for LLM backends.
//!
//! Each LLM provider (Anthropic, OpenAI, local models, etc.) gets a
//! parrot that translates chaos-abi into the provider's wire format
//! and back. Provider-agnostic by design — the kernel speaks chaos-abi,
//! parrots handle the dialects.
