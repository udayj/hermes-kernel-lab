# Hermes, Oxidized

A learning project exploring agent runtimes by growing a small Rust program.
Hermes is a capability reference and case study. The aim is to understand agent
mechanisms, state, and boundaries through a small implementation, without aiming
for feature parity or translating its architecture.

The CLI uses Anthropic’s Claude Haiku 4.5 (`claude-haiku-4-5-20251001`). It has
workspace tools, opt-in local skills and cross-session memory, automatic
conversation saving, and sandboxed command execution on macOS.

## Run

Set `ANTHROPIC_API_KEY` in the environment or a `.env` file in the current directory.
An existing environment variable takes precedence. API requests incur usage charges.

```sh
cargo run --
cargo run -- --workspace ./examples/workspace
printf '%s\n' 'Say hello in one sentence.' | cargo run --
```

Enter one message per stdin line. Blank lines are ignored; `/exit` or EOF exits.
Answers and tool results go to stdout; prompts, resume paths, and errors go to stderr.

## Workspace tools and permissions

`--workspace PATH` selects an existing, narrow project directory. Access is read-only
by default. `get_runtime_info` is available even without a workspace.

| Tool | Contract |
| --- | --- |
| `list_directory({path})` | One directory, sorted names and entry types; `.` selects the root. |
| `read_file({path})` | A regular UTF-8 file, at most 32 KiB. |
| `read_file({path, start_line, end_line})` | Both bounds required, one-based and inclusive; at most 1,000 lines and 32 KiB returned, with an 8 MiB scan bound. A start beyond EOF fails; an end beyond EOF returns available lines. Only the scanned prefix is validated as UTF-8. |
| `write_file({path, content, overwrite?})` | Publish at most 32 KiB of text atomically in an existing parent. `overwrite` defaults to false. |
| `patch({path, old_text, new_text})` | Atomically replace exactly one occurrence. Empty, missing, or ambiguous matches, including overlapping matches, fail without mutation. |
| `bash({command})` | macOS: a fresh noninteractive shell; returns after foreground execution and cleanup. |

The independent flags `--allow-workspace-writes` and `--allow-shell` require
`--workspace`. Writes enable `write_file` and `patch`; shell consent enables `bash`.
Shell consent alone permits writes only to private workspace scratch.
Enforcement applies to dispatch as well as advertised tools. Bash with both flags
can overwrite or delete ordinary workspace files: consent grants that authority
for the invocation, without per-command human approval.

Native paths are contained by a `cap_std::fs::Dir`. Relative symlinks within the
workspace may be followed; escaping paths fail. Dot-prefixed components and
resolved hidden targets are inaccessible. `AGENTS.md` cannot be written, including
through a symlink alias. Files must be regular UTF-8 files within the read bound.
Use `grep` through `bash` for search; it requires `--allow-shell`.

## Command boundary and lifecycle

A plain `--workspace` run uses native file tools and requires no Seatbelt or
external search executable. `--allow-shell` creates the macOS Seatbelt sandbox
and foreground runner. Initialization probes enforcement and fails closed, with
no unsandboxed fallback. Shell consent is rejected on other platforms.

The launcher uses fixed `/usr/bin/sandbox-exec` and `/bin/bash` paths. Its policy
denies access by default, permits workspace and required system reads, and allows
writes to private scratch or, with write consent, ordinary workspace files.
Dotfiles are inaccessible except for shell scratch; `AGENTS.md` is read-only.
Network and host socket/IPC access are denied. Descendants inherit the sandbox.

Each shell starts without startup files, with `/dev/null` stdin, a cleared
environment, controlled `PATH=/usr/bin:/bin`, and captured stdout/stderr. HOME,
temporary files, and caches use a private `.hermes-scratch-*` directory inside
the workspace. Scratch persists after exit and can be removed by the operator.
The parent stays outside Seatbelt for model HTTP, checkpoints, and memory.

Each tool call owns its command until execution and cleanup finish. The agent
then receives one result containing stdout, stderr, exit code or signal,
`timed_out`, and `truncated`. `subprocess` handles spawning, pipe draining,
process-group signalling, and reaping; `rustix` observes exit without reaping.
At most 32 KiB per stream is retained, with excess drained and discarded. Invalid
UTF-8 is replaced. Per-read time and byte bounds keep output floods from starving
the 30-second deadline.

Completion, timeout, or INT/TERM/HUP sends SIGKILL to the command's process group.
Output drains before the leader is reaped. Pipes that remain open after one second
of final draining produce a tool error. Signals exit the CLI after cleanup; while
idle they perform their default action. Resume never replays commands. There is
no background-job interface, PTY, interactive child stdin, or persistent shell.

The operator selects trusted checkpoint, memory, skills, `.env`, and offline-script
inputs outside the writable workspace. Model arguments, commands, and workspace
contents are untrusted. Concurrent filesystem mutation and pre-existing hard-link
aliases to outside files are unsupported. Parent SIGKILL and deliberately detached
descendants are outside process-group cleanup guarantees. There are no CPU, disk,
or process-count quotas. Workspace contents and output can enter model requests
and plaintext checkpoints.

## Offline demonstration

This macOS demo uses synthetic provider messages, temporary data, real tools,
and the compiled stdin CLI. It needs no credentials or live model connection.
Build before changing HOME so the normal Rust toolchain remains available.

```sh
cargo build --offline
demo_root=$(mktemp -d)
mkdir "$demo_root/workspace" "$demo_root/home"
printf 'amber\n' > "$demo_root/workspace/source.txt"
printf 'Run the synthetic workflow\n/exit\n' |
  HOME="$demo_root/home" ./target/debug/hermes-kernel-lab \
    --workspace "$demo_root/workspace" \
    --allow-workspace-writes --allow-shell \
    --offline-script examples/offline-workspace.json
cat "$demo_root/workspace/output.txt"
```

The fixture searches with bash/grep, reads, writes, patches `amber` to `blue`, runs an offline
shell check, reads the result, and answers. `output.txt` contains `blue`, and the
checkpoint is under the disposable HOME. `--offline-script PATH` consumes a JSON
array of synthetic Anthropic response objects, at most 1 MiB. It uses the same
response validation, request-size check, and turn budget as live operation.
It demonstrates orchestration and tool effects, not live model behavior.

## Instructions and local skills

Fresh sessions combine built-in guidance with root workspace `AGENTS.md`, if
present. `--instructions PATH` selects one workspace-relative UTF-8 file instead,
within the ordinary read limit. Reads happen once. Only a missing default file is
optional; other read failures stop startup. There is no nested/global discovery,
merging, or hot reload. Resume uses saved system text exactly and rejects
`--instructions`.

`--skills-dir PATH` independently selects one trusted local directory. It enables
`skills_list({})` for name-sorted metadata and `skill_view({name})` for one original
document. Only immediate `<name>/SKILL.md` files are loaded. Hidden root entries and
ordinary root files are ignored; visible directories must contain valid skills.
Symlink entries at the catalog root, escaping paths, and special files fail.
An empty catalog is valid; an invalid catalog
stops startup without partially loading it.

Documents have YAML frontmatter with exactly string fields `name` and
`description`, then a nonblank Markdown body. Names match their directory: 1–64
lowercase ASCII letters/digits separated by optional single interior hyphens.
Descriptions must be nonblank. Pinned `serde_yaml_ng` 0.10.0 supports normal scalar
forms; duplicate, unknown, missing, and collection-valued fields fail. This supports
the core [Agent Skills format](https://agentskills.io/specification), with optional
fields rejected and descriptions bounded by file/catalog limits.

The catalog and bodies are snapshotted once at startup. Metadata enters history
through listing, bodies through viewing; neither is injected into system text.
Procedures grant no permissions. Scripts, reference expansion, installation, and
skill editing are unsupported. Synthetic examples can be selected with
`--skills-dir ./examples/skills --workspace ./examples/workspace`.

## Memory and conversation state

`--memory-dir PATH` independently enables `memory_list({})`,
`memory_set({key, value})`, and `memory_delete({key})` for one fixed `memory.json`
in an existing trusted directory. Startup validates and loads it once. Missing
means empty; malformed, duplicate-key, oversized, symlink, and non-regular stores
fail locally. Entries enter context through tool results. Setting an unchanged
value or deleting an absent key is a successful no-op. Memory is plaintext;
deleting a fact does not erase copies in historical conversations.

Completed turns save under `$HOME/.hermes-kernel-lab/sessions/`; HOME must be an
existing absolute directory. The resume path prints after the first save.
`--resume-session PATH` restores validated system text and ordered history,
including correlated tool results. Historical tools are never replayed. Workspace,
write, shell, skill, and memory authority must be supplied again for current access;
saved history grants none. Skill snapshots are rebuilt for the new invocation.

Saving follows successful answer output and flushing. Turn/save failures stop the
program and preserve the previous checkpoint; saving can fail after an answer has
been displayed. No completed new turn means no checkpoint update. Checkpoints
must be trusted and contain plaintext conversation and tool output, without secret
redaction.

Native workspace writes, memory, and checkpoints synchronize a staged file and
publish atomically. New destinations refuse clobbering; Unix files use 0600.
The parent directory is not synchronized, so power-loss durability is not guaranteed,
and crashes may leave temporary files. One writer is assumed. Published writes and
command effects survive later model, output, or checkpoint failures: there is no
turn-level rollback or automatic command retry.

## Limits and checks

| Resource | Limit |
| --- | --- |
| Model calls per user turn | 8, one tool call per response |
| User message / shell command | 16 KiB each |
| Directory listing | 200 examined entries; 32 KiB serialized output |
| Skills | 20 documents; 32 KiB each; 32 KiB metadata listing; ordinary root enumeration limits |
| Memory | 32 entries; 64-byte keys; 1 KiB values; 32 KiB serialized file |
| Model request / response | 1 MiB each |
| Checkpoint | 2 MiB |
| Model output / HTTP timeout | 512 tokens / 60 seconds per request |

Invalid arguments, disabled tools, and unknown names return correlated tool errors.
Command exit failure is represented by its exit code/signal. Protocol, transport,
budget, and output failures stop the program. Invalid
or truncated responses never execute tools; a tool request on call eight fails
before execution. Every turn gets a fresh budget, including after resume.
Full history accompanies every request, without streaming, retries, compression,
or history truncation. A valid checkpoint can exceed the next request limit.

```sh
cargo fmt --check
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
```

Tests use synthetic responses and temporary files without credentials or external
network access. On macOS they exercise real Seatbelt, bash, network denial,
timeout/descendant cleanup, Ctrl-C, and one compiled offline workflow. The host must allow
Seatbelt initialization; nested sandbox restrictions can prevent these tests from
running. Other platforms do not run the macOS enforcement tests. Live-provider
behavior and whether a model follows skill procedures are not covered.
