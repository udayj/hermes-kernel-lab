# Hermes, Oxidized

A learning project exploring agent runtimes by growing a small Rust program,
with Hermes as a capability reference.

Currently, the CLI sends one or two user messages to Anthropic’s Messages API
using Claude Haiku 4.5 (`claude-haiku-4-5-20251001`) and prints each accepted
response. The second request includes the first user message, the first assistant
text response, and the follow-up, provided the first response is text-only.
Requests are non-streaming, with no retries. Every request advertises one client
tool, `echo`, which takes a required string argument named `text`.
No conversation is persisted between executions.

## Run

Create `.env` in the repository directory:

```dotenv
ANTHROPIC_API_KEY=your-key
```

The file is ignored by Git. An existing `ANTHROPIC_API_KEY` environment variable
takes precedence.

From that directory, run:

```sh
cargo run -- "Say hello in one sentence."
cargo run -- "Name a fictional planet." "Describe its sky in one sentence."
```

Pass one or two nonblank UTF-8 messages. The optional follow-up is supplied in
advance. These commands incur usage charges: one message makes one live model
call; two messages can make two. Both messages are validated before any call.
Text goes to stdout; errors go to stderr with a nonzero exit status.
Truncated, refused, empty, or unsupported responses produce an error without
printing partial text. If the first request, response, or output write fails,
the second request is not sent. A second-turn failure leaves the first response
already printed.

## Inspect a model-requested action

```sh
cargo run -- "Use echo to repeat: hello"
cargo run -- "Use echo to repeat: hello" "What happened?"
```

These are live API commands and incur usage charges. With `tool_choice: auto`
and `disable_parallel_tool_use: true`, the model may answer with text or request
at most one tool. A tool request displays its call ID, name, and JSON arguments,
along with any accompanying text in content order, labeled **requested, not
executed**. This is an accepted inspection outcome (exit status zero), not a
completed answer to the underlying task.

The program stops on a tool request on either turn. If the first response requests
a tool, it explicitly reports that the supplied follow-up was not sent. It never
executes `echo`, fabricates a result, sends a request after a tool request, or
persists the incomplete exchange. Text-only responses retain the existing flow.

Calls must have a nonempty ID containing only ASCII letters, digits, underscores,
or hyphens; the name must be `echo`; and the input must contain exactly the string
field `text` (an empty string is allowed). Malformed calls, multiple calls,
unsupported content, and inconsistent `stop_reason`/content combinations fail
without printing partial content. `tool_use` requires one valid call; `end_turn`
requires usable text and no call. Truncation and refusal remain errors.

Limits: 16 KiB per user message; 512 output tokens, a 60-second timeout, and a
1 MiB response body per request. Redirects are rejected.

## Checks

```sh
cargo fmt --check
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
```

Tests use synthetic data and make no API calls. Offline commands require cached
dependencies.
