# alcatraz-freebsd

FreeBSD backend using Capsicum-compatible process hardening and `procctl`.
Policy dimensions that cannot be enforced safely are rejected or reported by
the helper rather than silently treated as enforced.
