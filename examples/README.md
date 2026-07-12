# Examples

Each subdirectory is a self-contained pyo3 crate used to exercise Rylai's stub
generation end to end. Regenerate all stubs with:

```sh
just gen-pyi-examples
```

## Workspace membership

Most examples are listed in the root `Cargo.toml` `[workspace].members` so they
build alongside Rylai. The exceptions are noted below.

### `async_await_sample` — standalone workspace

This crate is **deliberately excluded** from the root workspace and carries an
empty `[workspace]` table in its own `Cargo.toml`. It depends on pyo3's
`experimental-async` feature (plus a tokio runtime), which pulls in machinery
that is irrelevant to Rylai's job (pure static AST analysis — no `cargo build`,
no Python runtime). Keeping it out of the root workspace prevents it from being
pulled into every `cargo build` / `cargo test` at the repo root.

Build it standalone when you actually want to run it:

```sh
cargo build --manifest-path examples/async_await_sample/Cargo.toml
```

Rylai itself only parses its source statically; you do not need to build it to
regenerate the stub (`just gen-pyi async_await_sample` works without cargo).
