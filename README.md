# Hermes, Oxidized

A learning project exploring agent runtimes by growing a small Rust program.
Hermes is both a capability reference and a low-fidelity design and
implementation reference: this project reconstructs selected mechanisms in a
smaller form without aiming for feature parity or translating its architecture
directly.

The CLI holds one in-memory conversation with Anthropic's Messages API using
Claude Haiku 4.5 (`claude-haiku-4-5-20251001`). Each user message starts a
bounded agent turn: the model may answer or request one enabled local tool, see
that result, and continue until it finishes. The next user message inherits the
complete structured history. Nothing persists between executions.

## Run

Create `.env` in the repository directory:

```dotenv
ANTHROPIC_API_KEY=your-key
```

The file is ignored by Git. An existing `ANTHROPIC_API_KEY` environment variable
takes precedence.

Run without a positional message for a line-oriented conversation:

```sh
cargo run --
cargo run -- --workspace ./examples/workspace
```

When stdin is a terminal, the prompt is written to stderr. Enter one message per
line. Blank lines are ignored; exact `/exit` and EOF stop successfully without a
model request. The current agent turn always finishes before the next line is
read. This is intentionally not a multiline editor or terminal UI.

Supply one message for a one-shot run that exits after the agent finishes:

```sh
cargo run -- "Say hello in one sentence."
```

More than one positional message is rejected. Text goes to stdout; prompts and
errors go to stderr. These commands make live API requests and incur usage
charges.

## Explore a workspace

`--workspace PATH` explicitly authorizes two additional read-only tools for that
directory:

- `list_directory` lists one directory as deterministically sorted names and
  entry types. `.` means the workspace root.
- `read_file` reads one regular UTF-8 text file and preserves its contents.

Paths from the model are workspace-relative. Absolute paths, parent traversal,
dot-prefixed components, and symlink traversal are rejected. Listings identify
symlinks without following them. Special files are not read. The tools cannot
write files, run commands, inspect the environment, or access a workspace when
`--workspace` was not supplied.

Enabling these tools allows selected file names and contents to be sent to the
model provider as tool results. Use a deliberately shareable workspace. Hidden
component exclusion covers names such as `.env` and `.git`, but is not automatic
secret discovery or full process isolation.

Example one-shot workspace exploration:

```sh
cargo run -- --workspace ./examples/workspace \
  "Discover the project files, read the worker configuration, inspect your runtime, and suggest a starting worker count with your assumptions."
```

## Agent and failure behavior

Assistant blocks and tool calls are retained in provider-visible history in
their original order. A local tool result immediately follows its matching
assistant call and uses the provider's call ID. Display labels such as
`[Local read_file result; call ...]` are terminal-only and never enter history.
File contents remain tool-result data, not system instructions.

Unknown or disabled tools, invalid arguments, missing or denied paths, invalid
UTF-8, and operation limits become correlated error results that the model can
respond to. Malformed protocol responses, multiple tool calls in one response,
inconsistent stop reasons, truncation, refusal, transport failures, request
budget failures, and output failures stop the program with a nonzero status.
The program does not retry or repair a failed session automatically.

Each actual user message permits at most eight model calls, including its first
call. A final answer on call eight succeeds; a tool request on call eight fails
before execution because no call remains to return the result. Tool results and
tool errors do not reset this counter.

Limits:

- 16 KiB per user message, enforced while reading stdin.
- 32 KiB per file.
- 200 examined entries and 32 KiB of serialized output per directory listing.
- 1 MiB serialized request body and 1 MiB response body per model request.
- 512 output tokens and a 60-second HTTP timeout per request.
- No redirects, streaming, retries, history truncation, or context compression.

Every request resends the complete growing history. The 1 MiB request limit is
a local byte ceiling, not a token estimate, provider-context guarantee, or
monetary budget. Longer conversations therefore cost progressively more input
tokens and eventually stop rather than being summarized.

`get_runtime_info` remains available without a workspace. It reports only the
binary target OS, target architecture, and `available_parallelism` estimate. It
does not inspect files, environment variables, host identity, or CPU load.

## Checks

```sh
cargo fmt --check
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
```

Tests use synthetic data and temporary workspaces and make no model API calls
or require credentials. They exercise input parsing, request and response
validation, tool catalog and dispatch behavior, and actual local filesystem
operations and bounds.

Scripted model responses exercise the same agent loop used by the CLI, with real
tool dispatch. These orchestration tests capture and assert the complete history
and tool definitions supplied on each model call. They cover direct answers,
correlated tool results, history inherited by a second user turn, recovery from
tool errors, the eight-call boundary and budget reset, and model-call and output
failures. Run just these tests with `cargo test agent::tests::scripted_`.
The synthetic responses bypass HTTP and response decoding; they do not verify
the integration between those layers and the loop. Filesystem-specific checks
run on the current Unix development host. Live-provider behavior, HTTP-driven
multi-turn behavior, and other operating systems are not exercised end to end.
