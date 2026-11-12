# chaos-pwd

Resolves the ChaOS home directory. Honors `CHAOS_HOME` if set,
otherwise uses `~/.chaos`. Validates the path exists and canonicalizes it.

`find_chaos_auth_home` resolves where provider credentials live: the
`CHAOS_AUTH_HOME` directory if set (it must exist; an invalid value is an
error, never a fallback), otherwise the ChaOS home itself.
