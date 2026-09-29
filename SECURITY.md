**English** | [日本語](SECURITY.ja.md)

# Security Policy

This document covers vulnerability reporting and the assurance boundary of Tsumugi. Before reporting, please read the "Assurance boundary" section below first.

## Project status

Tsumugi is an **alpha** release intended for educational and experimental use. Backward compatibility of the language specification, embedding API, and CLI is not guaranteed. Vulnerability handling is best-effort as well (this is a personal project, so no response timeline is promised).

## Assurance boundary (read before reporting)

**Tsumugi on its own is not a security boundary.**

Tsumugi's sandbox, path checks, step budget, and capability model are defense-in-depth (one layer among several), not a security sandbox that isolates untrusted code. To run adversarial scripts safely, you must combine Tsumugi with OS-level isolation such as a separate process, non-root execution, a minimal mount, and cgroups.

The threat model is the source of truth for what is guaranteed, what is not, and where responsibility is divided.

- [Threat model](docs/threat-model.md) (Japanese) — guaranteed properties / non-guaranteed properties / division of responsibility across host, Tsumugi, and OS
- [Tsumugi Manifesto](docs/manifesto.md) (Japanese) — design principles and non-goals

The following are known design non-goals, not vulnerabilities. Before reporting, please review section 8 "Properties not guaranteed" in the threat model.

- Fully isolating untrusted scripts with Tsumugi alone
- Recovery within the same process from OS-level OOM, stack overflow, or `panic=abort`
- A script intentionally emitting a secret that was legitimately passed to it
- Behavior of capabilities, execution budgets, or auditing in unimplemented phases (Phase 2 and later). See the [roadmap](docs/roadmap.md) (Japanese) for implementation status.

## Supported versions

Because this is an alpha release, security fixes are in principle applied only to the latest `main`.

| Version | Supported |
|---|---|
| Latest `main` | ✅ |
| Anything earlier | ❌ |

## How to report a vulnerability

**Please do not post vulnerabilities in public issues.** To avoid disclosing details before a fix is available, use GitHub's Private Vulnerability Reporting.

1. Open the repository's **Security** tab
2. Click **Report a vulnerability**
3. Describe the impact, reproduction steps, and expected severity

Including the following, to the extent possible, helps speed up handling.

- The affected engine (tree-walk / VM `--vm` / embedding API)
- A minimal reproducing script (`.tsg`) or steps
- Expected behavior versus actual behavior
- The relevant threat-model TM-ID, if any

## Handling process

- Once a report is received, review and discussion happen in the Private Vulnerability Reporting thread
- If confirmed as a valid vulnerability, a fix is made and, where appropriate, a GitHub Security Advisory is published
- No response timeline is guaranteed (best-effort)
