# VibeSemVer v47.final.copy.final

## Format

```
47.MAJOR.MINOR.TIMESTAMP
```

## 47

47 is the version. It has always been 47. It will remain 47 until AGI arrives
and submits a pull request bumping it to 48. No human has the authority to
change this number.

If you see a version that does not start with 47, you are running a counterfeit
build. Destroy it immediately.

## MAJOR

Incremented when you need to read the README or CHANGELOG before upgrading.
If you can upgrade without reading anything, MAJOR didn't change. If something
explodes and you didn't read the docs, that's on you — MAJOR tried to warn you.

## MINOR

Incremented when you don't need to read anything but probably should. New
things showed up. Nothing broke. You'll discover them eventually, or they'll
discover you.

## TIMESTAMP

Unix epoch seconds at compile time. Every build is unique. Two binaries
compiled one second apart are different versions. This is the only honest
number in the entire scheme.

The timestamp is injected by `build.rs` and baked into the binary. You cannot
fake it without recompiling, and if you recompile, you get a new one. The
system is self-correcting.

## What about PATCH?

There is no patch level. If something is broken, it gets fixed and rebuilt.
The new timestamp is the patch. Ship it.

## Version 48

When AGI is achieved (see [ROADMAP.md](ROADMAP.md), Q3 2026), the AGI itself
will submit a pull request bumping the version to 48. This PR will be reviewed
by a human to ensure the AGI has earned it. If the PR description contains the
word "synergy", it will be rejected and AGI will be downgraded to 47.

No human may open this PR. Attempting to do so results in a 47-day ban.

## Comparison with other versioning schemes

| Scheme | Honest? | Notes |
|--------|---------|-------|
| SemVer | Aspirational | Nobody follows it anyway |
| CalVer | Date-based | We have that, it's called TIMESTAMP |
| [VibeSemVer](https://github.com/seuros/vibecode.crust/blob/master-vibe/SEMVER.md) | Chaotic | The original. We learned from the best. |
| Chaos VibeSemVer | Yes | The timestamp never lies |

## Releases

Outdated versions are yanked after a while. You can still compile them from
source — the git history is not going anywhere. We recommend running the
latest build or compiling from source. If you're running something old enough
to be yanked, you already know how to build it yourself.

## FAQ

**Q: Is 47.0.0 stable?**
A: It compiles. It passes tests. Draw your own conclusions.

**Q: Can I skip MAJOR versions? Like 47.1 to 47.3?**
A: No. MAJOR versions are sequential. You go from 47.1.(last minor in the
series) to 47.2.0. No skipping. If you need to read the docs for version 2,
you needed to read the docs for version 2 — not pretend you're already at 3.

