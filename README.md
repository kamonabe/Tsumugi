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

Built-in step limits and filesystem restrictions are defense-in-depth, not a security sandbox that isolates untrusted code. See [SECURITY.md](SECURITY.md) for the assurance boundary.

## Quick start

```bash
# Build
cargo build

# Run a file
cargo run -- examples/hello.tsg

# Run in bytecode VM mode
cargo run -- --vm examples/hello.tsg

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
