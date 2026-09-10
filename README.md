# Hermes, Oxidized

A learning project exploring agent runtimes by growing a small Rust program,
with Hermes as a capability reference.

Currently, the CLI sends one or two user messages to Anthropic’s Messages API
using Claude Haiku 4.5 (`claude-haiku-4-5-20251001`) and prints each accepted
response. The second request includes the first user message, the first assistant
response, and the follow-up. Requests are non-streaming, with no tools or retries.
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
