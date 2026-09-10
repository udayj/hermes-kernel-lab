# Hermes, Oxidized

## Purpose
`hermes-kernel-lab` is a learning-first exploration of agent runtimes in Rust.
Hermes is a capability reference and case study, not a feature-parity target
or an architecture to translate. Focus explanations on agent mechanisms, state, boundaries, and design decisions.

Success means the owner can explain the current system and why it is shaped
this way. Prefer a small, understandable implementation over faster coverage.

## Scope and authorization
- Work on one explicitly approved step at a time. A roadmap, suggestion, or
  possible next step is not authorization to implement it.
- Before implementation is approved, propose the learning question, observable
  change, current success/failure policy, expected files, checks, and non-goals.
  Then stop without editing.
- Once approved, implement that step only. Do not add preparation for future
  capabilities, unused extension points, placeholder modules, or unrelated cleanup.
- If a necessary prerequisite or new risk materially expands the step, explain
  it and propose a smaller or revised scope rather than silently building ahead.
- Do not merge, begin the next step, or delegate parallel implementation unless
  explicitly requested. Do not change these instructions without approval.

## Design
- Each step should answer one main learning question. Supporting code and tests
  belong with that question; unrelated design changes do not.
- Prefer direct control flow, concrete types, and ordinary functions. Introduce
  abstractions only for a demonstrated current need, not predicted future use.
- Testability is a current need, but use the smallest seam that solves it.
- Add dependencies and modules only when the current step justifies them.
  Do not create a multi-crate workspace or a general framework in anticipation.
- Small, deliberate rewrites are acceptable when a new requirement teaches us
  why the existing design must change. Do not preserve provisional designs
  merely because they already exist.
- Simplify breadth, not correctness. Include the validation, bounds, and safety
  controls required by the behavior being introduced. Otherwise narrow it.
- Model only the protocol contract the current program needs. Use documented
  authoritative fields for decisions; add ancillary metadata or cross-checks
  only for a demonstrated current requirement.
- Distinguish necessary boundary handling from optional diagnostic polish.
  Optimize for understandable concepts and explicit decisions, not line count;
  retain the validation needed to know when the current promise cannot be fulfilled.

## Hermes and documentation
- Read current code and relevant current documentation before editing.
- Inspect Hermes only for the current learning question, not to predesign later
  subsystems. Do not scan the whole upstream codebase to plan the final system.
- When drawing on upstream, record the consulted commit and paths in the PR
  explanation. Distinguish observations, interpretations, and our own choices.
- Keep the README accurate about current behavior, how to run it, and important
  limitations. Keep the short learning narrative in the PR description.
  Do not create roadmaps or additional process documents unless requested.

## Verification and handoff
- Add focused deterministic checks for the changed behavior and relevant failure
  cases. Default tests must not require a live model, network, or credentials.
- Label synthetic fixtures honestly. Live checks are opt-in. Never print or
  commit secrets, and do not use personal data for demonstrations.
- Run `cargo fmt --check`, `cargo test`, and
  `cargo clippy --all-targets -- -D warnings` when applicable.
- Report what actually ran, what failed, and what was not checked. A passing
  command with no relevant tests is not evidence that behavior was tested.
- Explain the changed execution path, one relevant failure path, and any
  abstraction or dependency added. Give a runnable demonstration and one small
  exercise the owner can perform to check their understanding.
- Stop for review. Do not declare the owner's understanding or merge approval
  on their behalf, and do not automatically plan or implement the next step.
