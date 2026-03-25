# chaos-tmpfs

Async-safe LRU cache behind a Tokio mutex. Degrades to passthrough
when no runtime is present. Includes SHA-1 content hashing for
cache keys that survive file renames.
