# chaos-traits

Narrow trait abstractions that decouple the kernel from its satellite crates.
Defines the interfaces; implementations live elsewhere. Depend on this, not on
the concrete crates, to avoid circular dependencies.
