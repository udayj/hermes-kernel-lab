# Hermes, Oxidized

A learning project exploring agent runtimes by growing a small Rust program.
Hermes is both a capability reference and a low-fidelity design and
implementation reference: this project reconstructs selected mechanisms in a
smaller form without aiming for feature parity or translating its architecture
directly.

A small Rust agent CLI using Anthropic’s Claude Haiku 4.5
(`claude-haiku-4-5-20251001`), with read-only workspace tools, opt-in local skills,
opt-in cross-session memory, and automatic conversation saving.

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
available parallelism. Workspace and skill tools are read-only. Only the opt-in memory tools can write; there are no shell tools.

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

## Local skills

`--skills-dir PATH` authorizes one trusted local directory independently of
`--workspace`. It enables `skills_list({})`, which returns a name-sorted JSON array
of names and descriptions, and `skill_view({"name":"..."})`, which returns exactly
one original skill document. Without the flag, both tools are disabled. The model
selects a name, never a filesystem path. Skills do not enable workspace tools.

Only immediate `<name>/SKILL.md` documents are loaded. Hidden root entries and
ordinary root files are ignored; every visible immediate directory must contain a
valid skill. Symlink and special root entries are rejected. Reads beneath the
authorized root use the same capability-relative restrictions as workspace reads:
no symlink or parent traversal, and only regular UTF-8 files. An empty catalog is
valid. An invalid selected catalog stops startup locally, even on resume, before
credentials or model requests; no partial catalog is used.

The supported format is a YAML frontmatter mapping followed by a nonblank
Markdown body, with `---` delimiters on their own lines (LF or CRLF):

```markdown
---
name: workspace-overview
description: Summarize an authorized workspace using read-only tools.
---
List the workspace root and read README.md if present. Summarize what you find.
```

The mapping must contain exactly `name` and `description`, deserialized into Rust
strings using the YAML crate's normal behavior. Plain, quoted, and block scalars
are supported; numeric/boolean-looking scalars such as `123` and `true` are accepted
as text. Missing, duplicate, unknown, or collection-valued fields are rejected,
as are blank descriptions. Names must match the directory
name: 1–64 lowercase ASCII letters/digits, optionally separated by single interior
hyphens. YAML parsing uses pinned `serde_yaml_ng` 0.10.0; there is no custom YAML
parser. This follows the core [Agent Skills format](https://agentskills.io/specification),
but is not a full implementation: optional frontmatter fields are rejected, and
descriptions are bounded by our file/listing limits rather than the specification's
1,024-character limit. Markdown is retained verbatim, not interpreted by the runtime. Scripts,
reference-file expansion, skill writing, installation, and global/project discovery
are unsupported.

All selected documents are **snapshotted in memory at startup**, once per
invocation. Delivery to the model is on demand: listing exposes metadata only;
full documents enter history through `skill_view` results. Neither catalog nor
bodies are injected into the system prompt. Changes to disk do not affect the
current process; later invocations may load changed documents. Startup reads are
sequential, not an atomic snapshot of concurrent filesystem edits.

Fresh-session guidance treats skills as subordinate task procedures, not permission
grants or overrides of operator/user instructions. Ordinary file results remain
data. Skill results are printed and saved like other tool results and may be sent
to Anthropic. Loading a procedure does not guarantee that a model follows it.

Two deliberately synthetic examples use the existing workspace fixture:
`workspace-overview` lists the root and reads its README; `worker-config-review`
reads and explains `worker.toml`. With credentials configured, try:

```sh
printf '%s\n' \
  'List available skills, load worker-config-review, and use it to review worker.toml.' |
  cargo run -- --skills-dir ./examples/skills --workspace ./examples/workspace
```

Learning exercise: in an interactive stdin session, request a skill, edit its body
on disk, and request it again. Then resume with `--skills-dir` and request it once
more. Explain why the first process returns its original snapshot, while the new
process can return changed content alongside the unchanged historical result.

## Cross-session memory

`--memory-dir PATH` authorizes reading and writing one fixed `memory.json` in an
existing trusted directory, independently of workspace and skills access. Without
this flag, memory tools are disabled. Model arguments select text keys, never paths.
The versioned JSON store has the shape `{"version":1,"entries":{"label":"amber"}}`.

`memory_list({})` returns the entries as a key-sorted JSON object.
`memory_set({"key":"label","value":"amber"})` inserts or replaces one fact;
`memory_delete({"key":"label"})` removes it. Deleting an absent key or setting the
same value is a successful no-op. Limits are 32 entries, nonblank keys up to 64
UTF-8 bytes, nonblank values up to 1,024 bytes, and 32 KiB for the complete
serialized file, including JSON escaping. Accepted whitespace is preserved.

Startup loads and validates memory once, before credentials or model requests.
Missing memory means empty memory; malformed, unreadable, oversized, symlink,
and non-regular files fail locally without resetting the store. Duplicate keys
and unsupported versions are rejected. Startup, listing, and no-ops create no
file. Entries enter model context through tool results only, never system injection.
Guidance treats them as potentially stale data and directs mutations toward explicit
remember/correct/forget requests, with no automatic extraction. The CLI flag grants
write access: prompt wording is not an enforced per-entry approval mechanism.

With credentials configured, this synthetic demo uses two separate processes and
no resumed conversation:

```sh
mkdir -m 700 ./demo-memory
printf '%s\n' 'Remember label=amber using memory_set.' |
  cargo run -- --memory-dir ./demo-memory
printf '%s\n' 'Use memory_list and tell me the saved label.' |
  cargo run -- --memory-dir ./demo-memory
```

Memory and conversation saving are independent. Mutations validate the proposed
state before staging a file in the same directory, synchronizing it, and publishing
atomically (no-clobber for initial creation). Files use 0600 permissions on Unix.
Only publication updates the runtime state and permits tool success. Earlier
failures preserve the prior state and return a correlated tool error; the runtime
does not retry. A successful write survives later model, stdout, or checkpoint
failure, even when the previous checkpoint remains unchanged. The parent directory
is not synchronized, so power-loss durability is not guaranteed; crashes may leave
temporary files. One writer only: concurrent writers, directory replacement, and
external edits while running are unsupported.

Memory is **plaintext**, not a secrets vault. Tool results are printed, saved in
checkpoints, and may be sent to Anthropic. Deleting memory does not erase historical
copies in checkpoints. Resume preserves saved system/history exactly, requires
`--memory-dir` again for current access, and never replays historical writes.
Fresh sessions retrieve facts without importing old conversation history.

Learning exercise: run the demo, forget the label in a third process using
`memory_delete`, then inspect an older checkpoint. Explain why a fresh listing is
empty while the old conversation can still contain the label.

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

Likewise, supply `--skills-dir` again to enable new skill reads. Without it,
previous skill results remain in history but both skill tools are disabled.
With it, startup rebuilds the catalog from current files. Resume preserves the
saved system text exactly, including text from sessions predating skills; it does
not inject new guidance or replay historical loads. The checkpoint stores no
skill directory or catalog and grants no skill access.

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
| Skill root enumeration | Same directory limits, including ignored entries in the count |
| Skills catalog | 20 skills; 32 KiB per complete document; 32 KiB serialized metadata listing |
| Model request / response | 1 MiB each |
| Memory | 32 entries; 64-byte keys; 1 KiB values; 32 KiB serialized file |
| Checkpoint | 2 MiB |
| Model output / HTTP timeout | 512 tokens / 60 seconds per request |

Invalid tool arguments (including non-object inputs), unknown names, and disabled
tools return correlated tool errors to the model. Protocol, transport, budget, and output
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
covered by these checks. Skill tests verify metadata/body separation, bounded
snapshots, argument and filesystem failures, actual request histories through
list → view → workspace read → answer, and save/resume authorization. They verify
orchestration, not whether a live model follows a procedure correctly.
