# Maintainer question: bounded IPC for native macOS AGY ACP confinement

## Tested configuration

- Official Antigravity ACP 1.4.0, macOS arm64, macOS 14.8.9.
- Official Rust ACP SDK 3.3.0; stable ACP v1, no client filesystem or terminal.
- Explicit dedicated personal-OAuth profile; runtime-owned credentials.
- Temporary named global `PreToolUse` deny hook.
- Verified local HTTPS proxy with an exact provider-host allowlist and session
  CA; the proxy replaces generation system instructions authoritatively.

No OAuth tokens, headers, conversation IDs, profile paths, raw logs or private
model messages are included in this report.

## Reproduction and observations

1. Initialize ACP over stdio using the cleared subprocess environment.
2. Set the HTTP/HTTPS proxy and client-specific CA variables, including
   `HTTPLIB2_CA_CERTS`, to the same verified session CA.
3. Without OS confinement, `session/new` succeeds. The credential-free HTTPS
   control returns 404 and the runtime records three HTTP 2xx exchanges.
4. Apply a macOS Seatbelt policy permitting the exact proxy port and the
   session's Unix MCP socket, but not arbitrary local TCP binding/connections.
5. ACP initialization succeeds, but `session/new` returns `Internal error`.
   Sanitized error categories are permission failure, harness startup EOF and
   bind failure. The public control and three runtime HTTP 2xx exchanges still
   succeed. No model prompt is submitted.

Independent OS fixture tests prove that the configured proxy is reachable,
another loopback listener is unreachable, and new local TCP binding is denied.
The dedicated profile's policy is read-only; explicitly listed runtime-state
directories remain writable.

The distributed artifact's embedded code starts a local harness WebSocket and
a separate loopback credentials-injection HTTP server. Observing internal port
fields is not treated as a supported external configuration contract.

## Requested supported interface

Is there a supported ACP/runtime configuration for **both** IPC channels using:

- supervisor-selected fixed loopback endpoints, known before confinement; or
- private supervisor-selected Unix-domain socket paths?

The goal is to permit only those IPC endpoints plus the owned egress proxy,
without allowing unrelated local proxy/services to bypass canonical request
replacement and the provider-host allowlist. Authentication should remain
inside the official runtime, and Chaos should not patch private protocol
messages or replace Google's provider APIs.

If such a configuration exists, please identify its supported fields/flags and
version compatibility. Otherwise, is bounded supervisor-owned IPC planned?

## Non-goals

No request to bypass OAuth, use metered API keys, disable TLS verification,
remove native-tool policy, grant arbitrary loopback access or move execution
into a Linux VM. The deployment requirement is direct macOS execution.
