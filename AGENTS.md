# AGENTS.md

## Rust

This repository is a Rust-based coding agent, currently called `claw-code-rust`.
- All crate names use the `clawcr-` prefix. For instance, the crate in the `core` directory is named `clawcr-core`.
- When using `format!`, inline variables directly inside `{}` whenever possible.
- Always collapse nested `if` statements according to https://rust-lang.github.io/rust-clippy/master/index.html#collapsible_if
- Inline `format!` arguments whenever possible, following https://rust-lang.github.io/rust-clippy/master/index.html#uninlined_format_args
- Prefer method references instead of closures where applicable, per https://rust-lang.github.io/rust-clippy/master/index.html#redundant_closure_for_method_calls
- Avoid using `bool` or unclear `Option` parameters that lead to ambiguous calls like `foo(false)` or `bar(None)`. Instead, use enums, clearly named methods, newtypes, or other idiomatic Rust patterns that improve readability at the callsite.
- If such API changes are not feasible and positional literals must still be used, follow the `argument_comment_lint` rule:
  - Add an exact `/*param_name*/` comment before unclear positional literals (e.g., `None`, booleans, numeric values).
  - Do not include these comments for string or character literals unless they genuinely improve clarity, as these are exempt.
  - The comment must exactly match the parameter name in the function signature.
- Make `match` expressions exhaustive whenever possible, avoiding wildcard arms.
- Any newly introduced traits must include documentation explaining their purpose and how implementations should behave.
- In tests, favor comparing full objects rather than asserting on individual fields.
- If an API is added or modified, update the relevant documentation in the `docs/` directory when necessary.
- Avoid introducing small helper functions that are only used once.
- Keep modules reasonably sized:
  - Prefer creating new modules instead of expanding existing ones.
  - Aim to keep modules under 500 lines of code, excluding tests.
  - If a file grows beyond ~800 lines, place new functionality in a separate module unless there is a strong, documented justification not to.
- When running Rust-related commands (e.g., `just fix` or `cargo test`), allow them to complete without interruption, do not try to kill them using the PID. Slow execution due to Rust’s locking behavior is expected.
- Agent can not apply patch to a file more than 800 lines, cause windows patch length limit, agent would failed to apply patch.
- Do not introduce trivial wrapper functions — call the underlying function directly unless reuse or abstraction is clearly justified.

## Tests

### Test assertions

- Use `pretty_assertions::assert_eq` in tests to produce clearer diffs. Import it at the top of the test module if it’s not already present.
- Prefer deep equality checks by asserting entire objects instead of comparing fields individually.
- Do not mutate process environment variables in tests; instead, pass environment-dependent values or flags explicitly from higher levels.
- Tests that involve filesystem paths or other platform-dependent behavior MUST be platform-aware:
  - Use `#[cfg(windows)]` and `#[cfg(unix)]` to define platform-specific test cases when behavior differs.
  - Never rely on Windows-style paths being interpreted correctly on Unix, or Unix-style paths on Windows.
  - Always use platform-native path formats in tests so they align with `std::path::Path` semantics.