# FreeChaOS manual pages

This directory is the canonical source tree for the project's operator- and
model-facing manual pages.

## Adding a manual

Add a top-level file named `<name>.<section>.md`, with a section number from 1
through 9. Use letters, digits, hyphens, or underscores in the name. Every manual
must start with TOML frontmatter:

```toml
+++
title = "Terminal appearance"
summary = "Configure colors and user/assistant text styles."
+++
```

Follow the closing `+++` with the Markdown body. Both fields are required,
nonempty, single-line strings; unknown fields and malformed frontmatter fail
the build with the filename in the error. The filename supplies the stable
resource ID, so do not add an `id` field.

The kernel build automatically discovers, validates, sorts, and embeds these
pages. No Rust registry edit is needed. `README.md`, other non-manual filenames,
and subdirectories are not indexed.

- `chaos://man` lists titles, summaries, and resource URIs.
- `chaos://man/chaos-appearance.7` reads `chaos-appearance.7.md`, without its
  frontmatter or leading H1 title (the catalog already supplies the title).
  Relative links to other indexed manuals are rewritten to resource URIs;
  section headings and the rest of the body, including `SEE ALSO`, are preserved.
  A short footer links back to `chaos://man`; the full catalog is not repeated
  in every page.
  The original Markdown file retains its H1 for human readers.

Adding, editing, or removing a page triggers catalog regeneration on the next
build. Rebuild and restart ChaOS to expose the changes to models. Installed
binaries carry the pages and do not need this source directory at runtime.
These resources document features; they do not expose live configuration values.

In a packaged or installed system, these Markdown sources would typically map to
installed manpage paths such as:

- `share/man/man7/chaos-install.7`
- `share/man/man7/chaos-mcp.7`
- `share/man/man7/chaos-storage.7`
- `share/man/man8/chaos-httpd.8`

## Main pages

- [chaos-install.7](./chaos-install.7.md) — build, install, and logging
- [chaos-keyboard.7](./chaos-keyboard.7.md) — terminal UI keyboard shortcuts
- [chaos-appearance.7](./chaos-appearance.7.md) — terminal colors and message styles
- [chaos-providers.7](./chaos-providers.7.md) — provider configuration
- [chaos-support.7](./chaos-support.7.md) — support matrix (providers, OS, CI)
- [chaos-mcp.7](./chaos-mcp.7.md) — MCP client and server usage
- [chaos-modes.7](./chaos-modes.7.md) — collaboration mode discovery and switching
- [chaos-agents.7](./chaos-agents.7.md) — standalone agent role definitions
- [chaos-synopsis.7](./chaos-synopsis.7.md) — FreeChaOS sub-agent orchestration gate
- [chaos-attested-review.7](./chaos-attested-review.7.md) — protected independent review orchestration
- [chaos-storage.7](./chaos-storage.7.md) — runtime storage backend selection
- [chaos-halluacinate.7](./chaos-halluacinate.7.md) — Lua scripting engine

## Additional pages

- [chaos-httpd.8](./chaos-httpd.8.md)
