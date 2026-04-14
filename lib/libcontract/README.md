# lib/libcontract/

Internal contracts shared across chaos's own crates. Types and traits with no
business logic, kept separate so the kernel and its satellite crates can depend
on a common interface without pulling in each other's implementations.

Chaotic API — not a stable external surface; anything here can change between
chaotic releases.
External-facing contracts live under `include/`.

## Crates

- `abi`     canonical error and message types; all provider wire formats
            translate into and out of this shape
- `ipc`     protocol types for inter-process / inter-crate communication:
            rollout items, event messages, session metadata
- `traits`  narrow trait abstractions that decouple the kernel from concrete
            implementation crates
