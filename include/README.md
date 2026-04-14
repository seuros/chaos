# include/

External interface contracts. Traits and types that third-party code compiles
against — ports-tree provider packages, plugins, and external integrations.
No business logic, minimal dependencies, deliberately versioned.

Internal contracts shared only across chaos's own crates live under
`lib/libcontract/` instead.

## Crates

- `ration`  `UsageProvider` trait — providers report quota, rate-limit
            windows, and remaining credits back to chaos
