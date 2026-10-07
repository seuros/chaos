# Recall

**Persistent memory. Local retrieval. Accountable use.**

A PostgreSQL-native memory engine embedded in ChaOS—not a prompt dump or a
vector-search sidecar.

- **Hybrid retrieval:** concurrent semantic + lexical branches through Bonsai,
  RRF fusion, scoped deduplication, deadlines, cancellation, and degraded results.
- **Isolation by identity:** project/session namespaces; shared globals only
  when requested.
- **Receipt-gated reinforcement:** expiring session/runtime/scope/revision-bound
  receipts; atomic, capped updates, once per receipt/document. Exposure is not use.
- **Authority stays with the caller:** opt-in previews arrive as untrusted data.
  Source opening is permission-checked and race-safe.
- **Local inference:** lazy, process-shared Model2Vec with fingerprint-pinned
  artifacts. PostgreSQL/pgvector only. No silent SQLite fallback.

`recall_store → recall_search → recall_open → recall_use` · `recall_delete`

Relevance leads; charge breaks ties; scope/ID makes ordering deterministic.
UTC `created_at` survives upserts; `updated_at` follows content changes;
`last_used_at` follows newly applied use. Reads and replays never advance them.

Built on PostgreSQL’s battle-tested durability, transactions, and tooling—not
another hand-rolled storage engine. One shared foundation for the whole ecosystem.

No passive conversation ingestion. Model changes require explicit reindexing.

Let the host own identity, journals, and consolidation. Let the kernel own recall.

[API](src/lib.rs) · [Runtime](../kern/kern/src/recall.rs) ·
[Native tools](../kern/kern/src/tools/handlers/recall.rs)
