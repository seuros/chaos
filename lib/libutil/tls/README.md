# chaos-tls

Ensures exactly one rustls crypto provider is installed process-wide.
Disambiguates when both `ring` and `aws-lc-rs` show up in the dep graph.
