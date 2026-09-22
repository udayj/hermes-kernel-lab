# Hermes, Oxidized

A small Rust agent CLI using Anthropic’s Claude Haiku 4.5
(`claude-haiku-4-5-20251001`), with read-only workspace tools and automatic
conversation saving.

## Run

Set `ANTHROPIC_API_KEY` in the environment or a `.env` file in the current directory:

```dotenv
ANTHROPIC_API_KEY=your-key
```

An existing environment variable takes precedence. API requests incur usage charges.

```sh
cargo run --
cargo run -- --workspace ./examples/workspace
printf '%s\n' 'Say hello in one sentence.' | cargo run --
```

Enter one message per line on stdin. Blank lines are ignored; `/exit` or EOF exits.
Output goes to stdout; prompts, the resume path, and errors go to stderr.

## Workspace and instructions

`--workspace PATH` enables `list_directory` and `read_file` within that directory.
Paths must be workspace-relative; absolute paths, parent traversal, hidden path
components, and symlink traversal are rejected. Files must be regular UTF-8 text.
`get_runtime_info` is always available and reports target OS, architecture, and
available parallelism. There are no write or shell tools.

Fresh sessions use built-in system instructions plus one optional file:

- `--instructions PATH` selects a workspace-relative file and requires `--workspace`.
- Otherwise, `--workspace` loads its root `AGENTS.md` if present.
- Without a workspace or root `AGENTS.md`, only built-in instructions are used.

Instructions are read once, with the same restrictions as workspace files. Only
an absent default `AGENTS.md` is optional; other read failures stop startup.
There is no parent, nested, or global discovery, merging, or hot reload. Remove or
rename root `AGENTS.md` to omit it from new sessions.

Workspace names and file contents may be sent to Anthropic. Hidden-file exclusion
is not secret detection or full process isolation.

## Save and resume

Completed turns save automatically under `$HOME/.hermes-kernel-lab/sessions/`.
`HOME` must name an existing absolute directory. The resume path is printed after
the first successful save in each invocation; later turns update that file silently.

```sh
# First process:
printf '%s\n' 'Remember the label amber.' | cargo run --

# Second process: substitute the printed checkpoint path.
printf '%s\n' 'What label did I give you?' | \
  cargo run -- --resume-session /path/to/checkpoint.json
```

Resume restores the exact saved system instructions and complete ordered history,
including tool calls and results. It validates the version, provider/model, message
structure, matching tool results, and completed-turn boundary before any model
request. Historical tools are never replayed. Instruction files are not reloaded,
and `--instructions` cannot be combined with `--resume-session`.

Credentials and workspace access come from the current invocation. Supply
`--workspace` again to enable file tools; without it, saved instructions and file
results remain in history, but new workspace access is disabled. The checkpoint
path is independent of the workspace and does not authorize file access.

Checkpoints are **plaintext and must be trusted**. They contain conversation text
and may contain file contents or secrets included in that text. Credentials and
workspace permissions are not stored; transcript secrets are not redacted.

Saving happens only after a completed turn and successful stdout writes and flushes.
Turn or save failures preserve the previous checkpoint and stop the process; a save
failure can occur after the answer is displayed. Exiting without a completed new
turn does not create or update a checkpoint.

Writes use a synchronized temporary file in the same directory and atomic
publication. New destinations refuse overwrite; resume requires an existing
regular file and rejects symlinks. Newly created directories use 0700 permissions
and checkpoint files use 0600 on Unix. Existing directories must be trusted.
The parent directory is not synchronized, so publication is not guaranteed to
survive power loss. Crashes may leave temporary files. Concurrent writers,
automatic repair, and mid-turn recovery are unsupported.

## Limits and failures

| Resource | Limit |
| --- | --- |
| Model calls per user turn | 8, with one tool call per response |
| User message | 16 KiB |
| Workspace or instruction file | 32 KiB |
| Directory listing | 200 examined entries; 32 KiB serialized output |
| Model request / response | 1 MiB each |
| Checkpoint | 2 MiB |
| Model output / HTTP timeout | 512 tokens / 60 seconds per request |

Tool errors are returned to the model. Protocol, transport, budget, and output
failures stop the program. A tool request on the eighth model call fails before
execution. Each user turn gets a fresh call budget, including after resume.

Every request includes the full history. There is no streaming, retrying,
truncation, or compression; a valid checkpoint can exceed the next request’s
size limit. Longer conversations consume more input tokens.

## Checks

```sh
cargo fmt --check
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
```

Tests use synthetic data, scripted model responses, and temporary files; they
require no credentials or live model requests. Live-provider integration is not
covered by these checks.
