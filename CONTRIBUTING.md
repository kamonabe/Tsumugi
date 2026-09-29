**English** | [日本語](CONTRIBUTING.ja.md)

# Contribution Guide

Thank you for your interest in contributing to Tsumugi. This document summarizes the principles, workflow, and verification you should know when proposing a change.

Tsumugi is an **alpha** release for educational and experimental use, and backward compatibility of the language specification, embedding API, and CLI is not guaranteed. At this stage, we place particular emphasis on design consistency and reproducible verification.

## First: the design documents are the source of truth

In Tsumugi, **the design documents under `docs/` are the source of truth**, and the implementation follows them.

- Before changing the implementation, check the relevant spec under `docs/`
- For changes that alter a spec, update the documentation in the same PR as the code
- When in doubt, refer to the following (the documents below are in Japanese):
  - [Tsumugi Manifesto](docs/manifesto.md) — values, design principles, non-goals
  - [Design document](docs/design.md) — architecture of the current implementation
  - [Language specification](docs/language-spec.md) — observed spec of the current implementation (normative)
  - [Roadmap](docs/roadmap.md) — implementation status, progress, and the list of implementation gaps

Let the manifesto principle "prioritize host stability over execution speed" guide the whole effort.

## Development environment

- Language: Rust (edition 2024). For the MSRV, refer to `rust-version` in `Cargo.toml` (do not raise it unintentionally)
- Benchmarks: Criterion 0.5 (`benches/interpreter.rs`)

```bash
# Build
cargo build

# Run a file (tree-walk)
cargo run -- examples/hello.tsg

# Run in bytecode VM mode
cargo run -- --vm examples/hello.tsg

# REPL
cargo run
```

## Workflow up to submitting a change

The Git workflow is as follows. `main` is protected and cannot be pushed to directly.

```bash
# 1. Update main
git checkout main && git pull

# 2. Create a branch (the prefix follows the commit types below)
git checkout -b <prefix>/<short-description>

# 3. Make changes and commit
git add <files>
git commit -m "<type>: <summary>"

# 4. Push and open a PR
git push -u origin <branch-name>
```

### Commit message convention

| prefix | Use |
|---|---|
| `feat:` | New feature |
| `fix:` | Bug fix |
| `refactor:` | Refactoring |
| `docs:` | Documentation change |
| `style:` | Formatting only (no behavior change) |
| `ci:` | CI/CD configuration change |
| `deps:` | Dependency update |

## Verification (always run after changes)

Pass the same gates as CI locally before opening a PR.

```bash
cargo fmt --check              # Formatting (use cargo fmt to format locally)
cargo clippy -- -D warnings    # Lint; warnings are treated as errors
cargo test                     # Tests
```

Notes:

- CI's clippy looks only at the default target. To also check tests and benches, run `cargo clippy --all-targets -- -D warnings` locally
- In resource-constrained environments, reduce parallelism: `cargo test -j 1 -- --test-threads=1`
- Coverage is run in CI via `cargo llvm-cov` (not required locally)
- Tests run in CI on an ubuntu / macos / windows matrix. Watch out for OS-dependent behavior such as path separators and line endings

## The two engines (tree-walk / VM)

Tsumugi shares the Lexer, Parser, and AST, and has two execution engines.

- Default: tree-walk evaluator (**normative backend**)
- `--vm`: bytecode compiler + stack VM (experimental backend, with known differences from the default)

For changes that alter language behavior, **verify on both tree-walk and the VM**. Judge carefully before introducing any new difference between them, and confirm consistency with the list of implementation gaps in the [roadmap](docs/roadmap.md) (Japanese).

## Testing policy

- Add an integration test (`tests/`) or a unit test to new features and bug fixes
- For fixture-based tests, place a `.tsg` file and its expected output under `tests/fixtures/` and declare it with `fixture_tests!`; both the tree-walk and VM versions are generated
- For details on the test layout, see the "Tests" section of the [README](README.md) and `docs/design.md`

## Security

Report vulnerabilities via Private Vulnerability Reporting, not issues. For details and the assurance boundary, see [SECURITY.md](SECURITY.md).

## PR checklist

Confirm the following before opening a PR (the PR template has the same items).

- [ ] `cargo fmt --check` / `cargo clippy -- -D warnings` / `cargo test` pass
- [ ] For changes that alter language behavior, verified on both tree-walk and the VM
- [ ] For changes involving a spec change, updated the relevant document under `docs/`
- [ ] Added tests for new features and bug fixes
