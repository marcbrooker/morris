# AGENTS.md

## Code Quality

- Run `cargo fmt` after every change.
- `cargo clippy` must produce zero warnings.
- `cargo build` must produce zero warnings. Use `#[allow(...)]` only with justification.
- `cargo test` must pass before and after every change.
- Do not modify or remove existing tests unless explicitly asked.

## Style

- Follow existing code conventions in the file being edited.
- Keep functions short and focused.
- Use meaningful names; avoid abbreviations except where idiomatic in Rust (`cwd`, `buf`, etc.).
- Prefer returning `Result` over `unwrap()`/`expect()` in non-test code.
- Add doc comments (`///`) on public items and non-trivial internal functions.

## Architecture

- Morris uses a fixed, deterministic workflow. AI (Bedrock) is called exactly twice: once for mutation planning, once for analysis.
- All file I/O, test execution, and mutation application is deterministic code — no agent loops or tool-use protocols.
- The entire tool is a single binary in `src/main.rs`.
