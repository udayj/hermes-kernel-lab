# Hermes, Oxidized

A learning project exploring agent runtimes by growing a small Rust program,
with Hermes as a capability reference.

Currently, the CLI sends one or two user messages to Anthropic’s Messages API
using Claude Haiku 4.5 (`claude-haiku-4-5-20251001`). Each user turn can include
local `get_runtime_info` tool calls and further model requests until the model
finishes answering. The optional follow-up starts only after that completion,
carrying the complete history. Requests are non-streaming, with no retries.
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
advance. Both messages are validated before any call. These commands incur usage
charges: each completed user turn takes 1–8 model calls, so two completed turns
take 2–16 calls. A direct answer takes one call; one tool round followed by a final
answer takes two. Failures can stop earlier, including before any call.
Each request resends the growing history, increasing input-token costs. The call
limit is not an aggregate input-token or monetary budget.
Text goes to stdout; errors go to stderr with a nonzero exit status.
Truncated, refused, empty, or unsupported responses produce an error without
printing content from the rejected response or executing its tools. Transport
and output-write failures also stop unsuccessfully. Earlier output is preserved;
an output-write failure may leave a partially written display. If the first turn
fails, the supplied follow-up is identified as unsent and is not processed.

## Complete a tool round

```sh
cargo run -- \
  "Inspect your runtime. Report its target OS, architecture, and available parallelism." \
  "Based on those results, suggest a starting worker count for a CPU-bound task."
```

These are live API commands and incur usage charges. With `tool_choice: auto`
and `disable_parallel_tool_use: true`, the model may answer with text or request
at most one tool per response. The only tool is `get_runtime_info`, accepting
exactly an empty JSON object `{}`. It collects three fields only when executing
a validated model-requested call:

- `target_os`: `std::env::consts::OS`, the binary's target OS.
- `target_arch`: `std::env::consts::ARCH`, the binary's target architecture.
- `available_parallelism`: the estimate from `std::thread::available_parallelism`,
  or JSON `null` if unavailable. This describes parallelism available to this
  process, not physical cores or current CPU load, and is not a guaranteed optimal
  worker count.

These three fields are sent to Anthropic as the tool result. They are not
collected or injected into the initial prompt. The tool uses only the standard
library; it runs no shell or subprocess and inspects no files, environment
variables, hostname, user identity, or additional machine information.

After validating the complete response, the program displays accompanying
assistant text once and records all supported assistant blocks in their original
order. It executes the tool, immediately appends a user-role `tool_result` block
with the original call ID and the actual result serialized as a JSON string,
then calls the model again. Local results are displayed with a
`[Local get_runtime_info result; call ...]` label. Display labels never enter
model-visible history. Tool execution alone does not complete the turn.

Only an accepted `end_turn` completes the answer. The optional CLI follow-up then
inherits user messages, assistant text, tool calls, tool results, and final
answers. Text-only answers retain their plain-text output behavior.

Calls must have a nonempty ID containing only ASCII letters, digits, underscores,
or hyphens; the name must be `get_runtime_info`; and extra arguments are rejected.
Malformed calls, multiple calls,
unsupported content, and inconsistent `stop_reason`/content combinations fail
without printing partial content. `tool_use` requires one valid call; `end_turn`
requires usable text and no call. Truncation and refusal remain errors.

Each actual CLI user message has a fixed budget of eight model calls, including
the initial request. Tool-result messages do not reset the counter. An accepted
`end_turn` on call eight succeeds. A `tool_use` on call eight fails before display
or execution because no call remains to return the result. Truncated output is
never retried or recovered.

Other limits: 16 KiB per user message; 512 output tokens, a 60-second timeout, and
a 1 MiB response body per request. Redirects are rejected.

## Checks

```sh
cargo fmt --check
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
```

Tests use synthetic data and make no API calls. Offline commands require cached
dependencies. They cover request serialization, response validation, synthetic
runtime-result serialization (including `null`), ordered assistant blocks,
tool-result message construction, output errors, and the call-limit decision.
They do not exercise the HTTP-driven turn loop, repeated tool execution, or
follow-up scheduling end to end. HTTP integration tests and live checks are
outside this step's automated verification.
