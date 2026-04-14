# chaos-kern

The kernel. Schedules turns, manages sessions, routes events between components.
Intentionally thin — no built-in tools, no provider logic. All capability comes
from MCP drivers. If you are adding a feature here, ask whether it belongs in a
service or driver instead.
