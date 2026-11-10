+++
title = "chaos-trust(7)"
summary = "Inspect and provision project trust decisions without a terminal prompt."
+++

# chaos-trust(7)

## NAME

chaos-trust - inspect and manage project trust decisions

## DESCRIPTION

A trusted project loads its `.mcp.json`, rules, agents and project hooks. The
console asks once per directory. `chaos trust` is the operator interface for the
same decision where no console runs: hosted residents, fleet provisioning, and
moving an installation from SQLite to PostgreSQL. Decisions are stored per
installation, keyed by the exact canonical project directory.

## INSPECTION

```sh
chaos trust list
chaos trust show /srv/resident/identity
```

Both print JSON with this installation's identity and storage kind, and change
nothing. A decision is `trusted`, `untrusted` (an explicit refusal), or `none`.

## CHANGING ONE DECISION

```sh
chaos trust --yes set /srv/resident/identity trusted --expect none
chaos trust --yes revoke /srv/resident/identity --expect trusted
```

`--expect` is the decision you saw. If the stored decision differs when the
change is prepared or when it commits, nothing changes and the command fails,
so a stale script cannot overwrite a choice made since. The path must be an
existing absolute directory; it is stored under its canonical form, which the
receipt prints. The filesystem root and `CHAOS_HOME` are refused.

Every change prints a receipt (`chaos-project-trust-receipt/v1`): installation,
storage, before and after per path, and the time.

## MANIFESTS AND MIGRATION

```sh
chaos trust export > trust.json          # digest printed on stderr
chaos trust digest trust.json            # recompute after review
chaos trust --yes apply trust.json --digest v1:…
```

A manifest names one installation and lists decisions, denied ones included.
`apply` refuses a manifest for another installation, a digest that is not the
one you reviewed, duplicate or non-canonical paths, and any entry whose stored
decision differs from what the manifest expects. Without an explicit `expect`,
an entry applies only where nothing is stored or the stored decision already
matches. It is all or nothing: one conflict refuses the batch. Re-applying the
same manifest is a no-op.

To move the same installation from SQLite to PostgreSQL: export while the
bootstrap still points at SQLite, switch `storage_url` and transfer the data,
then apply the exported manifest. The installation identity lives in the Chaos
home, so it carries over; another home is another installation and is refused.
Manifests contain project trust only. Remembered execution approvals and hook
grants are never exported or imported here.

A fleet job writes one manifest per resident home (from that home's
`chaos trust list`), has it reviewed, and runs `apply` once per home with the
reviewed digest.

## AUTHORITY

Mutations need an interactive confirmation or explicit `--yes`. Agent shells
(`CHAOS_THREAD_ID` is set) are refused even with `--yes`: an agent cannot grant
itself project trust. Do not unset that marker to bypass the policy. As with
hooks, a process with direct shell and database access is not contained by this
interface; separate homes and OS permissions must enforce stronger separation.

## SEE ALSO

chaos-hooks(7), chaos-storage(7)
