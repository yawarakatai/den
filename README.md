# den

A lightweight Linux battery history recorder and live terminal monitor.
The monitor reads saved history read-only; it never writes a database.
The UI, help and diagnostics are in English.

## Status

An initial implementation: periodic recording, persistent history, battery and
power graphs, and baseline time-to-empty / time-to-full estimates. Neural-network inference,
model training, notifications and power-management actions are not implemented.
Den never changes charging thresholds or suspends the machine.

## Build and run

```sh
nix build .#den
nix profile install .#den
# Opens the live monitor; installing the package does not start a daemon.
nix run .
```

When working in a Git checkout, add new source files to Git before using these
flake commands: Git-backed flakes do not include untracked files.

The flake exposes packages for `x86_64-linux` and `aarch64-linux`. The existing
Plainix development shell is preserved.

For immediate live monitoring, no daemon or existing database is required:

```sh
cargo run
```

To also retain history across monitor sessions, without installing a service:

```sh
# Terminal 1: collect your actual battery data into the workspace.
cargo run -- --database target/trial.sqlite3 daemon

# Terminal 2: open the viewer.
cargo run -- --database target/trial.sqlite3
```

To record one snapshot, including on machines without batteries:

```sh
cargo run -- --database target/trial.sqlite3 daemon --once
```

For a synthetic snapshot:

```sh
cargo run -- --database target/fixture.sqlite3 daemon --once \
  --sysfs tests/fixtures/power_supply
# Inspect only the recorded fixture, without mixing in your actual sensors.
cargo run -- --database target/fixture.sqlite3 tui --history-only
```

A single snapshot is shown as a point; it is not enough for a time-to-empty
estimate. For a live fixture (constant values, not simulated load), pass
`--sysfs tests/fixtures/power_supply` to the monitor as well.

## NixOS service

Add the flake as an input. Until the project is published, a local input works:

```nix
inputs.den.url = "path:/absolute/path/to/den";
```

In a NixOS module that has access to `inputs`:

```nix
{
  imports = [ inputs.den.nixosModules.default ];

  services.den = {
    enable = true;
    sampleInterval = 30; # seconds
    retentionDays = 90;
    readers = [ "alice" ]; # replace with your login name
  };
}
```

This installs the CLI and configures a hardened systemd service running as the
non-root `den` user. It starts at `multi-user.target`, independently of logins.
The service has no network access and records to
`/var/lib/den/history.sqlite3`.

Readers join the `den` group and need a new login session for membership to take
effect. The state directory is mode `0750`; new database/lock files are mode
`0640` with the service's `0027` umask. Only the recorder can write history.
Battery history can reveal when the machine is used; access is opt-in.

After enabling the service, open `den`. Useful diagnostics:

```sh
systemctl status den
journalctl -u den
```

The package can also run on non-NixOS Linux. Automatic service setup outside
NixOS, a Home Manager module and a user-service unit are not provided yet.

## Live monitor

**Live monitoring and persistent recording have different rates:**

- While open, the monitor reads Linux battery sensors **every second**. It keeps
  up to ten minutes of high-resolution samples in memory, not on disk.
- The daemon independently records long-term history, normally every **30 seconds**.
  That interval does not limit the monitor's live updates.
- Saved history is checked every five seconds and joined with the live series.
  Live samples take precedence over overlapping saved samples.

The initial window is **60 seconds**, with power and charge graphs shown together.
A single sample has a visible endpoint; constant measurements still draw a line.
Charge axes auto-scale (explicitly labeled); the summary gauge always represents
0–100%. Small terminals show the focused graph only. An 80×24 terminal is
recommended. There is no header row: the summary reads **Battery health / Power /
ETA / Charge**, and `den` sits at the footer's right edge. The Power card uses
two lines: **Discharging 7.3 W** (state accented, watts white), then
**Battery · BAT0**. On tight layouts, long state names are abbreviated rather
than clipping the numeric value. The time-window selector is on the Power graph's
top-right border, with `←` / `→` hints (unavailable directions dimmed), and controls
both graphs; when only one graph is visible, it moves to that graph. Power stays
above Charge with matching time axes, even when resizing to a wide/full-screen
terminal. At 120+ columns and 24+ rows, a
right column appears automatically: **Battery use above, Details below**. Both
columns share the same row boundary, aligning Power with Battery use and Charge
with Details. The lower row stays compact (9–12 lines). Narrower/shorter terminals
hide the sidebar and use the full width for graphs. Expanding a graph uses the
full chart area without the automatic sidebar. `d` overrides sidebar visibility;
on narrow terminals it opens a Details overlay. The footer only advertises
`←→ Window`, `↑↓ Graph`, `? Help` and `q Quit` (Graph is omitted when space is tight).
Help is a small key table, with battery selection listed only when multiple
batteries are available. Diagnostic details appear there only on errors; longer
measurement explanations stay in this README. Stale sensors and history-only
mode remain visibly flagged in the footer without moving the charts.

The monitor never starts the recorder or creates a missing database. Missing,
inaccessible or busy history does not prevent live monitoring; the UI shows its
availability separately. It prefers the system history path, otherwise
`$XDG_STATE_HOME/den/history.sqlite3` or `$HOME/.local/state/den/history.sqlite3`.
A manually launched daemon defaults to the user-state location; the NixOS service
passes the system path explicitly. Use `--database PATH` to choose a history file.

Use `den tui --history-only` to disable live sampling and inspect a saved recording.
This mode anchors the graph at the last recording, is visibly labeled `HISTORY`,
and requires readable history. In normal live mode, a stopped recorder does not
freeze live graphs, but no high-resolution data survives closing the monitor.

| Key | Action |
| --- | --- |
| `Up` / `Down` | Select Power / Charge, also while expanded (no wrapping) |
| `f` | Expand/restore the focused graph |
| `Left` / `Right` | Select 60s, 5m, 15m, 1h, 6h, 24h or 7d |
| `b` | Select the next battery (help lists this only for multiple batteries) |
| `d` | Show/hide the right sidebar, or open Details on narrow terminals |
| `?` | Show the short key table and any active errors |
| `q`, `Esc`, `Ctrl-C` | Quit (`Esc` closes help or explicitly opened Details first) |

`Tab` (toggle graph), `-` / `+` (window) and `r` (manual history reload) remain
accepted for compatibility, but are not advertised in the TUI. History refreshes
automatically; routine viewing does not require a reload or graph selection.

### Hourly battery use

The right-hand **Battery use · Wh** chart compares the selected battery's outgoing
energy over 24 hourly UTC buckets, including the current, unfinished hour. This
is independent of the left graphs' window selector. The last bucket is not
extrapolated to a full hour. UTC is explicit so hour labels do not silently
change with daylight-saving time or require an extra runtime dependency.

It uses **saved recordings only**, including measurements taken while the TUI
was closed, and updates when new recordings arrive. Continuous discharging watts
are integrated with trapezoidal interpolation and split at hour boundaries.
Charging is excluded rather than subtracted from discharge; measured zero stays
zero. Unknown state, missing power while discharging, missing-battery heartbeats,
state transitions, restarts, suspend and recording gaps do not supply invented
energy. Energy-counter differences are not used as a fallback in this chart.

Bars stay cool white regardless of the current charging state. `Observed` is the
sum of energy over usable recorded intervals, **not an extrapolated 24-hour total**.
The scale is shown above the bars; hour labels are UTC. Markers below the bars are:

- `·`: no usable interval recorded, not zero usage.
- `~`: a past hour is only partially covered.
- `*`: the current hour has some usable recording and is still in progress.
- `0`: a fully covered past hour has zero outgoing battery energy.

Without saved history, the panel says so instead of fabricating bars. A single
snapshot cannot establish a measured interval. Very short panels show the
observed amount and ask for more height to display hourly bars. Battery use is
**not whole-PC consumption**: power supplied directly by AC is not included.

### What watts mean

**Battery flow is not total PC consumption or adapter input.** It measures energy
entering/leaving the battery. Firmware can temporarily report zero when switching
AC states; zero is also possible when full or charge-limited. A reported zero is
shown as such, never replaced by the last non-zero value. If `POWER_NOW` is zero
but measured current and voltage are available, their product supplies the rate.
Firmware may update its measurements more slowly than the one-second read rate.

Details prefers a measured `CURRENT_NOW`. If missing, current can be calculated
from finite nonnegative power and positive voltage in the same snapshot:
**A = W / V**, for example 7.3 W / 12.5 V ≈ 0.58 A. Calculated values are marked
`~0.58 A`; measured values have no `~`. Valid zero current remains visible, and
unusable/missing inputs yield `N/A`. This is display-only: the saved raw current
remains missing, not replaced by a calculated measurement. Wh alone cannot
supply instantaneous current.

The **Power** card and graph show unsigned watt magnitudes. Historical power
segments use **green for charging** (energy entering the battery) and **orange for
discharging** (energy leaving it); they retain their original state colors and do
not connect across state transitions. The card also names the state and
measurement source; `Power` does not imply whole-PC consumption.

The palette uses cool off-white primary values and titles, bright blue-gray
secondary information and axis labels, blue-gray borders, and white footer keys.
Muted colors are reserved for inactive or unavailable elements rather than all
supporting information. Valid `0 W` and `Paused` remain readable, not dimmed as
though unavailable. State colors are limited to indicators, the selected window,
focus border, charge gauge and power traces. The current accent is green
charging/full, a restrained copper orange discharging, and blue-gray paused/unknown.
Charge history is off-white, with only its current endpoint accented. Staleness takes
warning precedence; low-charge yellow/red overrides the charge value and gauge
independently of normal state accents. The terminal's background is preserved.
Missing values are `N/A`, not zero. Batteries with `Scope=Device` (for example,
peripherals) are excluded.

Graphs break at restarts, detected suspend intervals, sensor failures, missing
readings, wall-clock jumps and large gaps. Reduction preserves each screen-width
bucket's extrema. A live read failure immediately marks data stale and disables
ETA; recorded-only staleness uses three recording intervals or 90 seconds,
whichever is greater.

## Baseline prediction

The ETA card follows the battery state:

- **Discharging:** `Time to empty` uses remaining energy / recent discharge power.
- **Charging:** `Time to full` uses (reported full energy − current energy) /
  recent charging power. This targets the reported full capacity, not design capacity.
- **Full / Not charging:** displays `Full` / `Paused`, without a countdown.

Power is exponentially smoothed (120-second time constant, up to ten minutes of
continuous measurements in the same state). When power is unavailable, energy
changes can supply a fallback rate, using the appropriate direction. At least
three readings spanning 60 seconds are required; compatible saved history can
supply the initial window. State changes, suspend and interrupted history reset
warm-up. Stale sensors and unusable measurements do not produce an ETA.

Both are estimates **assuming a similar future power rate**, not promises.
Charging usually slows near full, so a constant-rate prediction can be optimistic;
charge limits below full capacity are not modeled yet. Empty means reported zero
energy, not a configurable OS shutdown threshold. Neither estimate includes a
calibrated uncertainty interval. Charge-only batteries have approximate Wh
conversions, marked with `~` in the UI; raw Ah values are also retained for later
analysis.

## PCs without a battery

The current collector reads system batteries only. On a batteryless PC the daemon
can record empty heartbeat frames, **not whole-PC consumption or a fabricated
zero-watt measurement**. CPU/GPU/whole-system power collection is not implemented yet.

Linux can expose other power sources, depending on the hardware:

- [RAPL / powercap](https://docs.kernel.org/power/powercap/powercap.html): differences
  between energy counters give average power for exposed CPU package and other
  domains. Domain coverage varies; parent and child domains overlap and must not
  simply be summed. Counter files may require privileges unavailable to the
  ordinary viewer or the current non-root service.
- GPU / hwmon power sensors: component power when the driver exposes it, not total
  PC input. Availability, units and access permissions depend on the device/driver.
- A reporting wall meter, metered UPS/PDU, or supported server BMC can provide
  whole-system readings, subject to the device's actual measurement scope.

A future collector should retain the source/domain and measurement scope, show
component power separately from battery flow, and work without charge/health/ETA
cards when there is no battery. CPU + GPU power is not a reliable substitute for
measured whole-PC consumption. Den does not change sysfs permissions or elevate
privileges to obtain these readings.

## Storage and operation

- SQLite, versioned schema, atomic measurement frames, foreign-key cleanup.
- A heartbeat frame is stored even when no system battery is present.
- One writer per database, enforced with an advisory lock.
- Raw measured fields are retained; predictions are not substituted for data.
- Retention runs at startup and approximately hourly. Deleted space can be reused;
  deleting records does not necessarily shrink the database file immediately.
- Rollback journaling is intentional: read-only viewers must also work after the
  writer exits, without writable WAL/shared-memory sidecars.
- Collection errors are logged and retried on the next interval. `--once` returns
  a failure status instead. After an unclean shutdown, the recorder may need to
  restart and recover SQLite before a read-only viewer can open history.
- No wake timers are installed. Suspend intervals are not sampled.

## Development

```sh
nix develop
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
nix flake check
```

For an end-to-end live test using only Python's standard library:

```sh
cargo build
python3 tests/live_pty.py target/debug/den
```

It drives the real executable in a pseudo-terminal, changes synthetic sysfs
values through discharge → charging/0 W → charging/32.4 W, checks visible colored
traces within seconds without a daemon, verifies unsigned power with preserved
historical green/orange state colors and changing accents, and tests sensor-error
recovery. It checks the two-line Power card, measured/derived/missing current,
bright cool-white primary values (including idle/paused readings), blue-gray
secondary text, compact contextual help, selector arrow hints, Up/Down graph
selection while expanded, shared window controls, and terminal restoration.
A second scenario seeds several hours of temporary discharge history, including
missing and partially recorded hours, followed by compatible charging history.
It checks the hourly Wh total, white bars, independent live-window controls,
numeric full-charge ETA, details toggle/Esc handling, and automatic sidebar
visibility when resizing between 200×50 and 100×32. Left graphs must remain
vertically stacked with matching widths, and both columns' panel borders align.
It never changes physical power settings. Diagnostic output is saved under
`target/den-live-*` and `target/den-charge-*`.

Rust tests use synthetic sysfs fixtures, temporary databases and an in-memory
terminal backend, including assertions on visible graph cells and colors rather
than only labels. The Nix checks build/test the package and evaluate service wiring.
Real hardware behavior, service sandboxing, group access and actual resource
consumption should also be checked before calling the service production-ready.

See [docs/design.md](docs/design.md) for design boundaries and prediction plans.
A project license has not yet been selected.
