+++
title = "chaos-prompts(7)"
summary = "Custom slash prompts stored in the runtime database."
+++

# chaos-prompts(7)

## NAME

chaos-prompts - reusable prompts available as `/prompts:<name>`

## DESCRIPTION

A custom prompt is a Markdown document. In the console, typing
`/prompts:<name>` inserts its body as the message; `$1`, `$2`, and
`$UPPER_SNAKE` placeholders are filled from the arguments you type after it.

Prompts live in the runtime database and are managed with `chaos prompts`:

```sh
chaos prompts import review.md       # stored under the file stem
chaos prompts list
chaos prompts show review
chaos prompts export review > review.md
chaos prompts disable review
chaos prompts remove review
```

Optional front matter gives the popup a description and an argument hint:

```markdown
---
description: "Review the given file for correctness"
argument-hint: "[file] [focus]"
---
Review $1 with attention to $2. Report findings with line numbers.
```

Changes apply to new sessions. A prompt whose name collides with a built-in
slash command is hidden. `${CHAOS_HOME}/prompts/` is no longer read.

## FILES

- runtime database, `user_documents` rows of kind `prompt`

## SEE ALSO

- [chaos-keyboard.7](./chaos-keyboard.7.md)
- [chaos-modes.7](./chaos-modes.7.md)
