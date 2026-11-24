# VibeSemVer v47.final.copy.final

## Format

```
47.MAJOR.MINOR+SHA[-dirty]
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

## SHA

`chaos --version` reports the abbreviated Git commit (at least eight hex
characters). Builds from the same clean commit report the same version.
Staged or unstaged changes to tracked files add `-dirty`; untracked files
alone do not. Builds without Git metadata report `+unknown`.

Published release tags still use `v47.MAJOR.MINOR.TIMESTAMP` so the updater
can order releases and install older tags. A source build has no release
timestamp, so replacing it with a release of the same package version
requires `chaos update --force`.

## What about PATCH?

There is no patch level. If something is broken, it gets fixed and rebuilt.
The new commit identifies the fix. Ship it.

## Version 48

When AGI is achieved, the AGI itself
will submit a pull request bumping the version to 48. This PR will be reviewed
by a human to ensure the AGI has earned it. If the PR description contains the
word "synergy", it will be rejected and AGI will be downgraded to 47.

No human may open this PR. Attempting to do so results in a 47-day ban.

## Comparison with other versioning schemes

| Scheme | Honest? | Notes |
|--------|---------|-------|
| SemVer | Aspirational | Nobody follows it anyway |
| CalVer | Date-based | Release tags still have a timestamp |
| [VibeSemVer](https://github.com/seuros/vibecode.crust/blob/master-vibe/SEMVER.md) | Chaotic | The original. We learned from the best. |
| Chaos VibeSemVer | Yes | The build identifies its source commit |

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
