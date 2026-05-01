# Contributing

Thanks for helping improve Tempyr.

## Development Setup

Install the stable Rust toolchain, then run the standard checks from the
repository root:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
cargo audit
gitleaks detect --source . --config .gitleaks.toml --redact --verbose
```

`cargo audit` and `gitleaks` are separate developer tools; install them before
running the full local suite.

The source install path targets the CLI crate:

```sh
cargo install --path crates/tempyr-cli --locked
```

## Pull Requests

- Keep changes focused on one behavior or documentation area.
- Add or update tests for user-visible behavior changes.
- Do not commit secrets, `.env` files, local agent settings, generated indexes,
  or rendered output unless a maintainer explicitly asks for them.
- When changing graph or journal behavior, update the relevant docs under
  `docs/`.

## Agent-Specific Files

The checked-in `.claude/skills` and `.claude/agents` files are examples for
Claude Code integration. Active hook settings are intentionally not committed;
copy the example from `docs/claude-settings.example.json` or generate them
locally with `tempyr init` when needed.
