+++
title = "chaos-planning(7)"
summary = "Durable workspaces, plans, tasks, and session attachments."
+++

# chaos-planning(7)

## NAME

chaos-planning - organize work across projects and sessions

## DESCRIPTION

Plans are saved independently of the conversation and remain available when you
resume work. A workspace groups projects; each plan contains tasks that can
target one or more of those projects.

PostgreSQL supports sharing plans across machines, nested tasks, and task
dependencies. SQLite supports local plans with nested tasks.
Tasks are not scheduled automatically, and changing a parent task's status does
not change its children. Several tasks may be in progress at once.

## REFERENCES

Use these short references to identify work:

| Record | Reference |
|--------|-----------|
| Workspace | Six uppercase hex digits, database-unique |
| Plan | Four uppercase hex digits, workspace-unique |
| Task | `T1`, `T2`, etc., allocated within its plan |
| Qualified task | `A3F092/00AF/T1` |

`T1` identifies a task, not its position in the list. Moving, renaming, nesting,
or cancelling a task never changes its reference. References are never reused.

Use a plan ID or `workspace/plan` reference to select a plan. A short task
reference requires an explicitly selected or attached plan.

## WORKSPACES AND CHECKOUTS

Create a workspace, register its projects, and bind their local checkout paths:

```sh
chaos workspace create ecosystem
chaos workspace list
chaos workspace register-project 000000 chaos
chaos workspace register-project 000000 skipper
chaos workspace projects 000000
chaos workspace bind PROJECT_ID /path/to/chaos
chaos workspace bind DRIVER_PROJECT_ID /path/to/chaos/drivers/skipper
chaos workspace locate
```

Use the IDs returned by the registration commands. Checkout paths are local to
each installation. A nested checkout, such as `drivers/skipper`, selects its own
project rather than the parent project.

On another machine using the same PostgreSQL database, select the existing
workspace/project IDs and bind that machine's paths. Plans remain readable
without local checkout bindings. Registering a project does not grant access
to its files.

`workspace list` and `workspace projects` accept `--offset`. Rename a workspace
with `workspace rename WORKSPACE REVISION NAME`.

## PLAN COMMANDS

Commands print JSON, with up to 50 records per page. Use the returned
`next_offset` or `next_after` value to request the next page.

```sh
chaos plan list 000000
chaos plan read 000000/0000
chaos plan read 000000/0000 --offset 50
chaos plan detail 000000/0000
chaos plan task 000000/0000 T1
chaos plan history 000000/0000
chaos plan history 000000/0000 --after 50
chaos plan attach SESSION_ID 000000/0000
chaos plan detach SESSION_ID
```

Plan and task details include their Markdown body and pending clarifications.
Task details also include the projects the task targets. History shows past changes.
Lists and `read` keep bodies out of the compact overview.

The `change` command accepts JSON. Reuse a `request_id` only
when retrying the identical operation:

```sh
chaos plan change '{"request_id":"create-1","action":"create","workspace":"000000","title":"Ship durable planning"}'
chaos plan change '{"request_id":"add-1","plan":"000000/0000","expected_revision":1,"action":"add_task","title":"Storage","body":"Persist plans and tasks. Verify restart recovery.","position":0}'
chaos plan change '{"request_id":"start-1","plan":"000000/0000","action":"transition","task":"T1","task_revision":1,"event":"start","reason":"Implementing storage"}'
```

Authoring actions are `create`, `edit`, `add_task`, `edit_task`, `move_task`, and `clarify`.
`edit_task` sets a title and the full list of project IDs. `add_task` and
`move_task` accept an optional parent and a separate sibling-order position.
Progress actions are `transition` and `note`; a note may target a task or the
whole plan. Plan lifecycle actions are `complete`, `cancel`, and `reopen`.

### Bodies and clarifications

`create` and `add_task` accept an initial Markdown `body`. To change the intent
later, append a clarification rather than replacing the body:

```sh
chaos plan change '{"request_id":"clarify-1","plan":"000000/0000","action":"clarify","task":"T1","text":"Also verify recovery on another machine."}'
chaos plan clarifications 000000/0000 --task T1
```

Omit `task` to clarify the whole plan. Clarifications preserve changes to intent;
use progress notes for work performed. Details show pending clarifications, with
`next_after` for further pages. `clarifications --after SEQUENCE` continues a page;
starting at zero includes the retained history of incorporated clarifications.

In Plan mode, the model can explicitly request a background rewrite:

```json
{"action":"consolidate","task":"T1","request_id":"rewrite-1","provider":"PROVIDER","model":"MODEL"}
```

Choose the provider and model from `chaos://models`. Appending a clarification
does not start a rewrite. The job receives only the item's title, body, and
captured clarifications, with no tools or conversation history.

Successful jobs publish the revised body automatically, without changing status,
ordering, or dependencies. Clarifications added while a job runs remain pending.
An outdated result is not published. Failed or cancelled jobs leave the body and
pending clarifications intact; retry explicitly with a new request ID.

```sh
chaos plan consolidation JOB_ID
chaos plan cancel-consolidation JOB_ID
```

The tool also returns a background task ID for `tasks://` reads and
`cancel_mcp_task` (omit `server`). Before executing a task, read its body and all
pending clarifications, as well as the plan's.

### PostgreSQL dependencies

`link` and `unlink` take prerequisite `parent` and dependent `child` references.
Nesting does not create a dependency. Dependencies must stay within one plan
and cannot form cycles.

```sh
chaos plan graph 000000/0000
chaos plan graph 000000/0000 --task T1
chaos plan validate-graph 000000/0000
chaos plan rebuild-graph 000000/0000 PLAN_REVISION
```

`--task` limits the graph to a task and its dependents. Graph and validation
reads accept `--offset`. `validate-graph` checks dependency data;
`rebuild-graph` repairs it without changing the declared dependencies.

## UPDATING WORK

Use the revision returned by the latest plan or task read when editing it.
If someone else changed it, your edit is rejected. Read it again and review
your change before retrying.

| Task event | Allowed transition |
|------------|--------------------|
| `start` | Pending → In progress |
| `block` | Pending/In progress → Blocked |
| `unblock` | Blocked → Pending |
| `complete` | In progress → Completed |
| `cancel` | Any unfinished state → Cancelled |
| `reopen` | Completed/Cancelled → Pending |

Status changes require a reason. Add notes to record context.
To complete a plan, first complete or cancel every task. Cancelling a plan
leaves its tasks unchanged. Reopen a completed or cancelled plan before editing it.

## MODEL TOOLS AND RESUME

| Session state | Planning tool |
|---------------|---------------|
| Plan mode | `plan`: discovery, reads, authoring, attachment, and progress |
| Execution, attached | `plan_progress`: reads, transitions, notes, complete/cancel, detach |
| Execution, unattached | No planning tool |

Plan mode allows editing plans, not project files. The model can switch to Plan
mode to revise an approved plan and return to execution afterward.

Attach a plan explicitly to work on it. It stays attached across mode switches
and session resume. Subagents receive a plan only when explicitly attached;
copying conversation history does not attach it.

Read a plan to refresh its view, including changes made on another machine.
JSONL reports updates as `plan.updated`. Finishing a chat turn does not complete
or detach a plan.

## SEE ALSO

chaos-modes(7)
