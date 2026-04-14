# chaos-storage

Backend-agnostic persistence layer. SQLite for the common case; Postgres when the
operator needs it. Everything that needs to survive a restart goes through here.
