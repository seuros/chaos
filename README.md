# Chaos

Chaos is an AI agent operating system. Not a coding assistant — an OS.
You pick the brain (OpenAI, Anthropic, local models), snap in the capabilities you need (modules),
and wire up external services (drivers). It was forked from OpenAI's Codex CLI after one too many
bugs were called features. It runs on a Celeron.

---

## Architecture

```mermaid
block-beta
  columns 3

  block:kernel["Kernel"]:1
    k1["LLM comms layer"]
    k2["OpenAI"]
    k3["Anthropic"]
    k4["Local models"]
  end

  block:modules["Modules"]:1
    m1["Tools that extend the OS"]
    m2["Voice"]
    m3["Sandbox"]
    m4["Hallucinate"]
  end

  block:drivers["Drivers"]:1
    d1["MCP servers for external services"]
    d2["File system"]
    d3["Telegram"]
    d4["Google Play"]
    d5["GitHub"]
  end

  style kernel fill:#1a1a2e,stroke:#e94560,color:#eee
  style modules fill:#1a1a2e,stroke:#0f3460,color:#eee
  style drivers fill:#1a1a2e,stroke:#16213e,color:#eee
```

**Kernel** — Talks to LLM providers. OpenAI, Anthropic, local models. This is the only
part that cares about wire protocols and API formats.

**Modules** — Extend what Chaos can do. Want voice? Module.
Want a custom tool for your workflow? Module. Everything is modular — Chaos is not
locked into being a coding agent.

**Drivers** — MCP servers that give Chaos its tools and connect it to the outside world.
File reading, shell access, Telegram, Google Play — if it speaks MCP, it's a driver.
Plug in, wire up, ship.

---

## Hardware Philosophy

Chaos runs on hardware you assemble from Temu parts. If it can't run on a Core 2 Duo
with 1 GB of RAM, it's out of tree.

Old hardware does not mean old software. Chaos expects bleeding-edge operating systems
and abuses every security primitive they offer:

- **Linux**: landlock, seccomp
- **FreeBSD**: capsicum
- **OpenBSD**: pledge, unveil
- **macOS**: sandbox profiles

No shims. No compatibility layers. If the OS gives us something, we use it.

Windows is not supported.

---

## Clamping / Docking

Anthropic requires MAX subscribers to use the official Claude Code harness.
The Clamping module works within these terms: it launches Claude Code with `--bare`,
strips its built-in tools, and connects through MCP. Chaos provides the tools.
Chaos hooks into the lifecycle. Claude Code becomes the transport.

API key users connect directly through the kernel — no clamping needed.

This architecture is correct usage of both providers' terms of service.

---

## Install

See [Installing & building from source](./docs/install.md).

---

## Docs

- [Contributing](./docs/contributing.md)
- [Installing & building from source](./docs/install.md)

---

## Origin

Chaos was forked from [OpenAI Codex CLI](https://github.com/openai/codex).
The fork exists because upstream refused to fix bugs and called them features.
The codebase has since diverged significantly — Chaos is provider-agnostic,
modular, and built for hardware that most projects have forgotten.

---

Licensed under [Apache-2.0](LICENSE).
