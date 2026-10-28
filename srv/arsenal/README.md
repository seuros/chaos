# chaos-arsenal

Tool registry and search. Indexes available tools and answers queries about what
is available to the current process. The kernel asks the arsenal; it does not
maintain its own tool list.

## Filesystem inspection

Prefer filesystem tools over shell pipelines:

- `read_file`: bounded slices or indentation-aware blocks.
- `grep_files`: regex search with text and 1-based line numbers.
- `locate_files`: names and globs.
- `list_dir`: bounded listings.

Enable `filesystem` if needed. Run independent calls in parallel.

### Content search

`grep_files` defaults to `output_mode: "content"`:

```json
{
  "pattern": "def (json_error_result|current_company)",
  "path": "app/controllers",
  "include": "*.rb",
  "context_lines": 2,
  "limit": 30
}
```

Matches contain absolute `path`, 1-based `line_number`, `text`, and `truncated`.
Optional `context_before`/`context_after` arrays contain numbered context.
Results deduplicate hits per line; multiline matches include each covered line.

`output_mode: "files"` returns paths only. `limit` counts lines or files
(default 100, cap 2000); `match_count` counts returned results, not total hits.
If `has_more`, repeat the query with `offset: next_offset`.

Context: default 0, max 20 lines per side. Lines cap at 500 UTF-8 bytes;
pages cap at 128 KiB of serialized matches. Long lines retain the first hit;
`…` marks omitted prefixes. Both caps preserve pagination.
Order: newest file first, then path; source order within files.
Pagination requires unchanged files and modification times.

Search respects ignore rules; skips symlinks, binary files, and files over
10 MiB. No shell commands.
