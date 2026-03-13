# Contributing to Rylai

Thank you for your interest in contributing. Please follow the guidelines below.

## Pre-commit checks with prek

Before each commit, run the project’s hooks using **[prek](https://github.com/j178/prek)** (a fast, Rust-based pre-commit runner compatible with our config).

From the repository root:

```bash
prek install
```

This installs the Git hook so that **every commit** runs the configured checks automatically.

### What runs on each commit

The hook runs the same checks as in `.pre-commit-config.yaml`:

| Hook          | Purpose                                      |
|---------------|----------------------------------------------|
| `check-yaml`  | Validate YAML syntax (e.g. GitHub workflows) |
| `check-toml`  | Validate TOML syntax (e.g. `Cargo.toml`)     |
| `cargo-fmt`   | Format Rust code with `cargo fmt --all`     |
| `cargo-check` | `cargo check --workspace`                   |
| `cargo-clippy`| `cargo clippy --workspace --all-targets -- -D warnings` |

If any step fails, the commit is aborted. Fix the reported issues and try again.

## Development workflow

1. Fork the repo and create a branch from `master`.
2. Run **prek** before committing: `prek run --all-files` or rely on the Git hook after `prek install`.
3. Commit and push; open a pull request.
4. Ensure CI (build, test, lint on Ubuntu, Windows, macOS) passes.

## CI

Pull requests must pass the GitHub Actions workflow (see `.github/workflows/ci.yml`): build and test on Ubuntu, Windows, and macOS, plus lint (format, check, clippy). Running prek locally should keep your branch aligned with CI.

---

If you have questions, please open an issue.
