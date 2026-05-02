# Installing Tempyr

Tempyr is a Rust workspace. The repo root `Cargo.toml` is a virtual manifest, so source installs need to target `crates/tempyr-cli` instead of `cargo install --path .`.

## Linux

Run:

```bash
bash install.sh
```

The script first builds Tempyr in release mode:

```bash
cargo build --release --manifest-path crates/tempyr-cli/Cargo.toml --locked --bin tempyr
```

Then it installs Tempyr with:

```bash
cargo install --path crates/tempyr-cli --root "${XDG_DATA_HOME:-$HOME/.local/share}/tempyr" --locked --force --bin tempyr
```

It then updates `PATH` idempotently in `~/.profile` and in the active shell's rc file when that shell is `bash` or `zsh`.

If you want `install.sh` to skip shell startup file changes, pass `--no-path-update`:

```bash
bash install.sh --no-path-update
```

## Windows

Run:

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

The script first builds Tempyr in release mode:

```powershell
cargo build --release --manifest-path .\crates\tempyr-cli\Cargo.toml --locked --bin tempyr
```

Then it installs Tempyr with:

```powershell
cargo install --path .\crates\tempyr-cli --root "$Env:LocalAppData\Tempyr" --locked --force --bin tempyr
```

It then updates the user `PATH` so new shells can find `tempyr.exe`.

If you want `install.ps1` to skip user `PATH` changes, pass `-NoPathUpdate`:

```powershell
.\install.ps1 -NoPathUpdate
```

## Updating safely

Rerun the installer to update Tempyr. Both installers run the release build before checking whether the target Tempyr binary is already in use, so compile failures do not interrupt a currently running installed binary. If the target binary is in use, they only stop processes whose executable path exactly matches the target installed binary. They do not kill processes based on name alone.

If the binary becomes locked during the install anyway, the installers stop matching Tempyr processes and retry. On Windows, the installer also waits and retries a few times before failing when the lock appears to be transient.

## Custom install root

Both installers accept a custom install root:

```bash
bash install.sh --install-root /some/path
```

```powershell
.\install.ps1 -InstallRoot C:\Some\Path
```
