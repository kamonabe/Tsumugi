**English** | [日本語](README.ja.md)

# Tsumugi (紡ぎ)

A project aiming to be a controllable, embeddable scripting language implemented in Rust.

> This is a short English overview. The full documentation is in Japanese: see [README.ja.md](README.ja.md) and the design documents under [`docs/`](docs/).

## What Tsumugi aims to be

Tsumugi began as a personal project to deepen understanding of programming language implementation, and is growing into research and development of an embeddable scripting language with real-world use in mind.

> Tsumugi aims to be a scripting language that, when embedded in server applications and business systems, runs business rules and extension logic predictably and auditably, within the permissions and execution budget the host explicitly grants.

It prioritizes host stability over raw execution speed, making steady progress under controlled load. For the design principles and non-goals, see the [Tsumugi Manifesto](docs/manifesto.md) (Japanese).

> [!IMPORTANT]
> The manifesto describes the direction Tsumugi aims for. It does not mean the current implementation guarantees all of those properties.

## Status

Tsumugi is currently a dynamically typed language with Ruby-like syntax, in an **alpha release for educational and experimental use**. Backward compatibility of the language specification, embedding API, and CLI is not guaranteed.

It shares a Lexer, Parser, and AST, and runs either with the default tree-walk evaluator or with a bytecode compiler + stack VM (`--vm`, an experimental backend with known differences from the default).

Implementation status at a glance (the authoritative per-item status is the status table in the [roadmap](docs/roadmap.md), in Japanese):

- **CLI:** has capability profiles (`safe` by default, `legacy` for compatibility) and options such as `--allow-*`, `--fs-*`, and `--allow-import-root`. Under `safe`, every external effect except stdout (file operations, environment variables, `now()`, `input()`, `exit()`, and imports) is denied unless granted by an option. Run `tsumugi --help` for the full list.
- **Embedding API:** two surfaces coexist until they are unified: the minimal alpha facade (`Engine`, `CompiledScript`, `ExecutionContext`, `ExecutionOutcome`) and the Phase 1/2 embedding API (`compile` → `link` → `run`, exported from the crate root as `EmbeddingEngine`, `EmbeddingContext`, and so on). Both start from an empty, deny-by-default capability set.
- **Capabilities (Phase 2):** a deny-by-default `CapabilitySet` (filesystem roots, environment snapshot, clock, stdin/stdout, process exit, module resolver) and a host function registry are implemented; some Phase 2 acceptance criteria remain open.
- **Execution budgets and cooperative execution (Phase 3/4):** comprehensive budgets (fuel, collections, strings, source/import counts) plus deadlines and cancellation work on both backends; yield/resume, the cooperative scheduler, and cooperative host calls are partially implemented.
- **Runtime audit (Phase 6):** opt-in. Audit events (schema v1) are emitted only when an audit sink is configured through the embedding API, on the tree-walk backend only. The CLI does not emit audit events.

Built-in budgets and capability restrictions are defense-in-depth, not a security sandbox that isolates untrusted code. See [SECURITY.md](SECURITY.md) for the assurance boundary.

## Quick start

```bash
# Build
cargo build

# Run a file
cargo run -- examples/hello.tsg

# Run in bytecode VM mode
cargo run -- --vm examples/hello.tsg

# Imports need an import root to be granted
cargo run -- --allow-import-root default=examples examples/import_demo.tsg

# Grant individual capabilities (denied by default under the safe profile)
cargo run -- --allow-env HOME --allow-clock script.tsg

# REPL (interactive mode)
cargo run
```

A small example (`.tsg`):

```
let name = "tsumugi"
print(f"hello, {name}")

fn add(a, b)
    return a + b
end

print(add(3, 4))
```

For the language syntax and semantics, see [LANG_GUIDE.md](LANG_GUIDE.md) (English), which is written for AI code assistants and includes the formal grammar.

## Documentation

The full README and the design documents are maintained in Japanese.

- [README.ja.md](README.ja.md) — full overview: embedding API, supported features, tests, CI, project layout
- [LANG_GUIDE.md](LANG_GUIDE.md) (English) — language guide with the formal grammar
- [`docs/`](docs/) (Japanese) — design source of truth (manifesto, language spec, design, threat model, roadmap, and more)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). In short: `main` is protected, so work on a feature branch and open a PR, and pass `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test` before submitting.

## License

MIT. See [LICENSE](LICENSE).
