+++
title = "chaos-trust(7)"
summary = "Inspect and provision project trust decisions as the operator over chaos serve."
+++

# chaos-trust(7)

## NAME

chaos-trust - inspect and manage project trust decisions

## DESCRIPTION

A trusted project loads its `.mcp.json`, rules, agents and project hooks. The
console asks once per directory. The operator trust tools make the same
decision where no console runs: hosted residents, fleet provisioning, and
moving an installation from SQLite to PostgreSQL. Decisions live in the
database the home's `storage_url` names, keyed as described under CHANGING ONE
DECISION.

The trust table has no installation column. Every home that points at the
same database shares its decisions: a grant made through one home is a grant
for all of them. Give each Chaos home its own database (a home per OAuth
account is a separate home). A manifest's installation identity is checked
against the home applying it, which keeps a manifest from crossing homes; it
does not isolate homes that share a database.

The tools are served by `chaos serve --mcp` at `/mcp`, and only there:

```sh
CHAOS_BEARER_TOKEN=… chaos serve --mcp --bind 127.0.0.1 --port 4000
```

| Tool           | Does                                                        |
|----------------|-------------------------------------------------------------|
| `trust_list`   | every stored decision, with installation and storage kind   |
| `trust_show`   | the key a directory is decided under, and what is stored    |
| `trust_set`    | grant (`trusted`) or revoke (`untrusted`) one decision      |
| `trust_export` | every decision as a manifest, with its digest               |
| `trust_plan`   | what a reviewed manifest would change, without writing      |
| `trust_apply`  | apply a reviewed manifest                                   |

`trust_list`, `trust_show`, `trust_export` and `trust_plan` change nothing.
A decision is `trusted`, `untrusted` (an explicit refusal), or `none` (never
decided). `none` can be read and expected but never written.

## CHANGING ONE DECISION

`trust_set` takes `project_path`, `decision` and `expect`, the decision you
saw. If the stored decision differs when the change is prepared or when it
commits, nothing changes and the call fails, so a stale script cannot
overwrite a choice made since.

A decision sits on the key the trust gates read: the Git root of the
directory, or the directory itself outside a repository. Hooks and onboarding
use the same key. A grant on a subdirectory or worktree inside a repository is
refused (and held, on apply) with the root it belongs to. A denial there is
refused too, unless a decision is already stored under exactly that path: the
configuration loader reads exact-directory decisions, so an old grant left on a
directory that has since moved into a repository can still be revoked in place.

The guards restrict grants only. A grant needs an existing directory outside
`CHAOS_HOME` that does not contain it. A denial needs only an absolute,
normalised path, so a grant on a directory that contains `CHAOS_HOME` (such as
`~`) or on a directory that has since been deleted can still be revoked; a
path that no longer resolves is revoked by its exact stored key, and only if a
decision is stored there. The filesystem root is always refused, and held on
apply.

`revoke` stores `untrusted`; it does not delete the decision. A manifest
exported before the revocation therefore finds a denial where it expects
nothing or its own grant, and is refused instead of restoring trust.

Every change returns a receipt (`chaos-project-trust-receipt/v1`): installation,
storage, actor, before and after per path, and the time. Each change also
writes a `configuration_events` row (`project_trust_trusted` or
`project_trust_untrusted`) in the same transaction, naming the path, before,
after, installation, actor and manifest digest. Onboarding writes record the
actor `onboarding` and the home's installation identity if it has one; an
unreadable identity fails the write. Reading decisions never creates an
installation identity; a home without one has nothing of its own to export.

## MANIFESTS AND MIGRATION

`trust_export` returns `{manifest, digest}`, and refuses an installation with
more than 10,000 decisions, the most a manifest can carry. `trust_plan` and `trust_apply`
take the manifest exactly as reviewed and that digest.

A manifest names one installation and lists decisions, denied ones included.
`apply` refuses a manifest for another installation, a digest that is not the
one you reviewed, duplicate or non-canonical paths, and any entry whose stored
decision differs from what the manifest expects. Without an explicit `expect`,
an entry applies only where nothing is stored or the stored decision already
matches.

A grant that fails the grant guards (its directory is missing, it is not its
project's root, it contains or sits inside `CHAOS_HOME`, or `CHAOS_HOME` does
not resolve) is *held*, and so is any entry for the filesystem root: it is
not written, not compared and not rechecked, and the plan and the receipt list
it under `held` with the reason. Every other entry is the committing set. That
set is all or nothing: one conflict in it refuses the batch, and each of its
entries, the ones that change nothing included, is checked again inside the
committing transaction, so a receipt never reports one as it was reviewed if
it moved before the commit. A receipt with held entries records a partial
apply. `trust_apply` refuses a manifest with any held entry unless the call
passes `allow_held: true`; a migration that must carry every decision should
never pass it, and should run `trust_plan` before stopping the resident.
Re-applying the same manifest is a no-op.

To move the same installation from SQLite to PostgreSQL: run `trust_plan` on
a fresh export first; stop or drain the resident and keep it stopped; start
`chaos serve --mcp` on loopback against the home while its bootstrap still
points at SQLite, export, and stop it; switch `storage_url` and transfer the
data; start serve again against the new database and apply the exported
manifest with its digest. Nothing on the source side needs PostgreSQL. A
resident started on the new database before the apply comes up with no
decisions, so its projects get the implicit read-only default; explicit
sandbox settings, permission profiles and requirements still apply as
configured. The digest binds the exported file, not choices made on the source
after it was written; a source left running can decide again and the manifest
will not know. The installation identity lives in the Chaos
home, so it carries over; another home is another installation and is refused.
Manifests contain project trust only. Remembered execution approvals and hook
grants are never exported or imported here.

A fleet job writes one manifest per Chaos home (separate homes, such as one
per OAuth account, are separate installations), has each reviewed, and runs
`trust_apply` once per home with that home's reviewed digest. Hook approval is a
separate step (chaos-hooks(7)).

## AUTHORITY

The tools exist only on the HTTP endpoint, never on the stdio server
(`chaos mcp`), and every call must come from the principal the operator bearer
authenticates. Serve refuses to start if its shell environment policy would
pass `CHAOS_BEARER_TOKEN` to the commands or MCP servers it starts; the default
`*TOKEN*` exclude keeps it out. That does not hide the credential from a
process that can read serve's own environment or arguments, so pass it in the
environment rather than with `--bearer-token`, and sandbox agents so they
cannot inspect serve. The same bearer authorizes triggers and live monitoring:
any client holding it can also change trust. Each
change is audited with the operator and the MCP session as actor. As with
hooks, a process with direct shell and database access is not contained by
this interface; separate homes and OS permissions must enforce stronger
separation.

## SEE ALSO

chaos-hooks(7), chaos-storage(7), chaos-httpd(8)
