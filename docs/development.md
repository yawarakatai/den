# Development

[Back to the README](../README.md) · [Design](design.md)

## Build and test

```sh
git clone https://github.com/yawarakatai/den
cd den
nix develop
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked
python3 tests/live_pty.py target/debug/den
nix flake check --no-build --all-systems
nix build .#checks.x86_64-linux.package .#checks.x86_64-linux.module --no-link
```

The existing Plainix development environment is preserved. Git-backed flakes
exclude untracked files; add new source/license files to Git before Nix checks.
On an aarch64 host, replace `x86_64-linux` with `aarch64-linux` in the build command.
Without Nix, a recent stable Rust toolchain, C compiler and Python 3 suffice for
the Cargo and PTY checks.

Rust tests use synthetic sysfs trees, temporary databases and an in-memory
terminal backend. The real PTY test uses only Python's standard library and
changes synthetic sensors through discharge, charging, not-charging, full and
read failures. It checks visible cells/colors, live setting refresh, read-only
history, hourly usage, estimates, resizing, controls and terminal restoration.
Diagnostics are saved under `target/den-*`.

The Nix package check builds/tests the Rust package. The module check **evaluates
service wiring**, not a booted VM or a deployed system service. CI configures
native x86_64 and aarch64 package/module checks plus Rust/PTY checks; a configured
job is not evidence of success until it has run on GitHub.

Actual unplug/replug, suspend/resume, group permissions, service sandboxing and
resource profiling need separate real-hardware validation. Do not enable a
service, change device permissions or alter power settings just to run tests.

## Synthetic and manual trials

```sh
# Terminal 1: record actual battery readings in the ignored workspace directory.
cargo run -- --database target/trial.sqlite3 daemon
# Terminal 2:
cargo run -- --database target/trial.sqlite3
```

For a synthetic snapshot:

```sh
cargo run -- --database target/fixture.sqlite3 daemon --once \
  --sysfs tests/fixtures/power_supply
cargo run -- --database target/fixture.sqlite3 tui --history-only
```

One snapshot is not enough for an estimate or hourly usage. For a constant live
fixture, pass `--sysfs tests/fixtures/power_supply` to the viewer as well.
Use synthetic data for screenshots and bug reports; personal recordings can
reveal usage patterns. Do not commit databases, terminal dumps or local state.

## Publication checklist

- Keep Cargo/Nix versions and MIT metadata consistent; include `LICENSE`.
- Check relative documentation links and the synthetic README screenshot.
- Run the checks above and inspect `cargo package --list --locked --offline`.
  Package contents should contain no personal history or generated captures.
- Confirm CI results on the public repository, including native aarch64 builds.
- Keep experimental prediction and real-deployment limitations visible.
- Create tags/releases or publish packages only as a separate explicit step.
  GitHub/Nix distribution does not require publishing the `den` crate on crates.io.
