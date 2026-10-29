# den

A lightweight Linux battery history recorder and live terminal monitor.
The monitor reads saved history read-only; it never writes a database.
The UI, help and diagnostics are in English.

## Status

An initial implementation: periodic recording, persistent history, battery and
power graphs, live power-setting details, and experimental time-to-empty / time-to-full
scenario ranges. Neural-network inference,
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
Both graph axes auto-scale; the Charge summary gauge always represents 0–100%.
Small terminals show the focused graph only. An 80×24 terminal is recommended.
There is no header row: the summary reads **Power / Charge / ETA / Battery health**,
and `den` sits at the footer's right edge. Every summary card has two content lines
(four rows including borders). Charge puts its percentage and remaining Wh together
above the gauge, saving one row across the normal-width summary. Narrow layouts
use two rows of cards. The Power card uses
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
Details shows battery measurements and live platform/CPU power settings, without
routine recording cadence or save-age information. Recorder problems remain available in help, not as a permanent
Recording section. Help is a small key table, with battery selection listed only
when multiple batteries are available. Diagnostic details appear there only on errors; longer
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

### Power settings (live)

Details reads these kernel controls every five seconds, without root, D-Bus or
external commands:

- **Platform:** firmware power mode from `firmware/acpi/platform_profile`.
- **Driver:** CPU frequency driver (`scaling_driver`).
- **EPP:** CPU energy/performance preference; `balance_performance` is displayed
  as `balance perf.` and `balance_power` as `balance power`.
- **Pstate:** Intel/AMD pstate mode (`active`, `passive`, etc.), when the CPU driver
  identifies a supported pstate interface.
- **Governor:** CPU frequency governor (shown when the panel has enough height).

These are separate settings, not one interchangeable “profile.” In particular,
Platform reports the firmware's actual control, **not** a claimed TLP or
power-profiles-daemon selection. Den does not guess the controlling manager from
AC state or a governor. CPU controls are checked across all `cpufreq/policy*`
policies: differing known values show `Mixed`; incomplete, inaccessible or
unsupported values show `N/A`, not a remembered previous value.

This first version is **live display only**: settings are not stored in SQLite
or used to condition predictions. History-only mode does not read current power
settings and says they were not recorded. A custom `--sysfs` tree ending in
`class/power_supply` uses the same tree's root for these controls; other fixture
layouts leave them unavailable rather than reading the host's CPU settings.
Nothing here changes system power settings or starts a power-management service.

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
segments use **blue for charging** (energy entering the battery), **copper orange
for discharging** (energy leaving it), and **green for Full**. They retain their
original state colors and do
not connect across state transitions. The card also names the state and
measurement source; `Power` does not imply whole-PC consumption.

The palette uses cool off-white primary values and titles, bright blue-gray
secondary information and axis labels, blue-gray borders, and white footer keys.
Muted colors are reserved for inactive or unavailable elements rather than all
supporting information. Valid `0 W` and `Not charging` remain readable, not dimmed as
though unavailable. Power and Charge traces/endpoints, the Charge gauge and UI
accents use **state colors**: blue Charging (`#8FB5E8`), restrained copper orange
Discharging (`#D18B47`), green Full (`#8BC592`), and blue-gray not-charging/unknown.
The ETA's `Full` status is also green; ordinary W, %, health and time-range values
stay white. There are no remaining-level color
bands. Both histories retain the state at each measurement, break at state
transitions and never repaint past history from the current reading. Hourly
Battery use bars remain white. Staleness takes warning precedence in UI accents;
the Charge number alone retains low-charge warnings (red below 15%, yellow below
30%). The terminal's background is preserved.
Missing values are `N/A`, not zero. Batteries with `Scope=Device` (for example,
peripherals) are excluded.

Graphs break at restarts, detected suspend intervals, sensor failures, missing
readings, wall-clock jumps and large gaps. Reduction preserves each screen-width
bucket's extrema. A live read failure immediately marks data stale and disables
ETA; recorded-only staleness uses three recording intervals or 90 seconds,
whichever is greater.

## Experimental prediction ranges

The ETA card follows the battery state:

- **Discharging:** `Time to empty` uses remaining energy / recent discharge power.
- **Charging:** `Time to full` uses (reported full energy − current energy) /
  recent charging power. This targets the reported full capacity, not design capacity.
- **Full / Not charging:** displays `Full` / `Not charging`, without a countdown.
  These are driver-reported states, not inferred from the charge percentage.
  A configured charge limit can normally stop charging below 100% while the
  driver reports `Not charging` rather than `Full`. The same state can also occur
  for other reasons, so den does not infer that the limit was reached. Charge
  limits themselves are not currently collected.

Power is exponentially smoothed (120-second time constant, up to ten minutes of
continuous measurements in the same state). When power is unavailable, energy
changes can supply a fallback rate, using the appropriate direction. At least
three readings spanning 60 seconds are required; compatible saved history can
supply the initial window. State changes, suspend and interrupted history reset
warm-up. Stale sensors and unusable measurements do not produce an ETA.

The card displays a **recent-use scenario range** in two complementary rows,
with readable spacing when the width permits:

```text
In  15m – 40m
At  21:23 – 21:48
```

**In** is the remaining-duration range in white/bold; **At** is the corresponding
local 24-hour clock range in blue-gray. Both use the same outward-rounded bounds
and the selected reading's timestamp, never a separate current clock or an old
recording projected from the viewer's present time. At shows only clock times,
without day suffixes; In provides the duration needed to interpret midnight or
multi-day ranges. Labels and spaces may be omitted when needed, never the
endpoints. A range spanning a UTC-offset change is explicitly shown in UTC to
avoid ambiguous or reversed local clocks. If a complete clock range cannot be
converted or safely fit, At shows `N/A` while In remains available. An open-ended
In range produces an open-ended At range, not a fabricated upper time.

Warm-up/unavailable readings keep their explanatory note instead of a clock
range. Full stays green and Not charging stays white, without a countdown or projected
time. The baseline smoothed rate remains the center scenario. Continuous
measured watts are trapezoidally integrated into complete 60-second mean rates; the 10th/90th
percentiles supply slow/fast scenarios. A minimum **±20% rate margin** around the
baseline prevents constant readings from implying perfect certainty. The energy-
trend fallback uses that margin alone. Remaining/unfilled energy divided by fast
and slow rates gives the time range, rounded **outwards to five minutes**. Very
long bounds use compact hours; a slow scenario exceeding seven days is left
open-ended (`≥…`), not capped as a promised maximum.

A one-row scenario histogram was trialled but removed: sparse floating blocks
without labelled axes were ambiguous in this small card. ETA instead pairs the
duration range with its projected clock range. A future distribution view would
need more space and clear axes, not an unlabelled curve beside the estimate.

**This is an uncalibrated heuristic, not an 80% confidence/probability interval.**
The observed rate percentiles do not establish the coverage of future runtimes.
Neither point accuracy nor interval coverage has been validated. It assumes
roughly similar future use, and the user can change load at any time. Charging
usually slows near full, so even the range may be optimistic; taper and charge
limits below full capacity are not modeled. Empty means reported zero energy,
not a configurable OS shutdown threshold. Charge-only batteries have approximate
Wh conversions, marked with `~` in the UI; raw Ah values are retained for later
analysis. Prediction results never replace raw measurements or run in the daemon.

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
historical blue/copper/green state colors and changing accents, and tests sensor-error
recovery. It checks state-colored Charge history/gauges, Power/Charge/ETA/health
ordering, white In/secondary At ranges without ambiguous histogram blocks,
non-UTC local-time conversion, matching duration/time bounds, green Full status,
the absence of routine ETA subtitles, two-content-line summary cards,
live power-setting refresh, absence of
routine recording details, measured/derived/missing current,
bright cool-white primary values (including idle/not-charging readings), blue-gray
secondary text, compact contextual help, selector arrow hints, Up/Down graph
selection while expanded, shared window controls, and terminal restoration.
A second scenario seeds several hours of temporary discharge history, including
missing and partially recorded hours, followed by compatible charging history.
It checks the hourly Wh total, white bars, independent live-window controls,
the full-charge scenario range, details toggle/Esc handling, and automatic sidebar
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
