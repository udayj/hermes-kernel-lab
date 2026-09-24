---
name: workspace-overview
description: Summarize the purpose and top-level contents of an authorized workspace.
---
This is a synthetic learning example using read-only workspace tools.

1. Use `list_directory` with `{"path":"."}` to inspect the workspace root.
2. If the listing includes `README.md`, read it with `read_file`.
3. Summarize the stated purpose and top-level entries. Distinguish observed
   contents from assumptions; do not infer the contents of unread files.
4. If workspace tools are unavailable, explain that workspace access is needed.
