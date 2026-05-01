# Release Process

1. Run the full local check suite:

   ```sh
   cargo fmt --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace --locked
   cargo audit
   gitleaks detect --source . --config .gitleaks.toml --redact --verbose
   ```

2. Update `CHANGELOG.md` with user-facing changes.
3. Confirm the managed Claude settings example matches the embedded asset:

   ```sh
   git diff --no-index --exit-code docs/claude-settings.example.json crates/tempyr-cli/assets/claude.settings.json
   ```

4. Confirm install scripts still work on their target platforms:

   ```sh
   bash install.sh --no-path-update
   powershell -ExecutionPolicy Bypass -File .\install.ps1 -NoPathUpdate
   ```

5. Tag the release:

   ```sh
   git tag -a v0.1.0 -m "Tempyr v0.1.0"
   git push origin v0.1.0
   ```

6. Publish release notes from the changelog entry.
