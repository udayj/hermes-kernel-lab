# Hermes, Oxidized

A learning project exploring agent runtimes by growing a small Rust program,
with Hermes as a capability reference.

Currently, the CLI sends one user message to Anthropic’s Messages API using
Claude Haiku 4.5 (`claude-haiku-4-5-20251001`), prints the text response, and exits.
Requests are non-streaming, with no conversation history, tools, or retries.

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
```

This makes a live API call and incurs usage charges. Pass exactly one nonblank
UTF-8 message. Text goes to stdout; errors go to stderr with a nonzero exit status.
Truncated, refused, empty, or unsupported responses produce an error without
printing partial text.

Limits: 16 KiB input, 512 output tokens, a 60-second timeout, and a 1 MiB response
body. Redirects are rejected.

## Checks

```sh
cargo fmt --check
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
```

Tests use synthetic data and make no API calls. Offline commands require cached
dependencies.
