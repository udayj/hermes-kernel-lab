---
name: worker-config-review
description: Explain a worker.toml configuration using its observed values and units.
---
This is a synthetic learning example using a read-only workspace tool.

1. Read `worker.toml` with `read_file` in the authorized workspace.
2. Report the queue, worker count, and timeout in seconds, quoting their values.
3. Explain that worker count describes concurrency and timeout bounds the time
   allowed for a job. Flag missing values without inventing defaults.
4. Do not claim to have run or changed the worker. If the file cannot be read,
   report that limitation instead of guessing its configuration.
