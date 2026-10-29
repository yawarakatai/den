# Design

## Decisions

- Linux only; English UI, CLI help and diagnostics.
- One binary: `den` / `den tui` for live monitoring, `den daemon` for recording.
  A missing/stopped recorder must not prevent live monitoring.
- Standard deployment: one machine-wide system service, running as a dedicated
  non-root user; authorized interactive users only read the history.
- Nix flake package, application output and NixOS module. Preserve Plainix's
  development environment. No automatic host installation or service activation.
- Defaults: one-second live sensor sampling while the monitor is open, 30-second
  persistent recording, 90-day retention, five-second saved-history checks.
  The live graph defaults to a 60-second window, not a multi-hour window that
  compresses a few new samples into the last column.
- Headerless UI: four summary cards (health, power, ETA, charge), and branding at
  the footer's right edge. The Power card has two content lines: accented state
  beside white watts, followed by the battery source. Tight widths abbreviate the
  state rather than clip the value. The shared window selector has explicit left/
  right arrows (dimmed at unavailable ends) in the Power chart's top-right border,
  following the visible chart
  when expanded or space-constrained. Power and Charge remain vertically stacked
  with matching time axes, not moved into separate columns when maximized. Wide
  terminals (120+ columns, 24+ rows) automatically show Battery use above Details
  in the right column. Both columns use one shared vertical split, aligning Power
  with Battery use and Charge with Details. The lower row is limited to 9–12 lines
  so metadata does not stretch through a sparse full-height column. Narrow/short
  terminals hide the sidebar by default. `d` explicitly overrides visibility and
  uses a Details overlay on narrow terminals; expansion hides the automatic sidebar.
  The palette uses bright cool-white primary values, including valid zero and
  paused readings, brighter blue-gray secondary information/axis labels, blue-gray
  borders, and white footer keys instead of dimming all supporting text. Green
  charging/full and restrained copper orange discharging accents are limited to
  selection, focus, state labels, the gauge and historical power traces. Charge
  history stays off-white; only its current endpoint takes the accent. Hourly
  battery-use bars stay cool white independently of the live state. Past power
  colors and low-charge warnings are independent of the current accent. The
  footer only advertises window, graph selection, help and quit, not every optional
  action. Up selects Power and Down selects Charge without wrapping, including in
  expanded mode. Normal help is a content-sized key table; battery selection
  appears only for multiple batteries, diagnostics only on errors. Explanatory
  prose belongs in README. Legacy Tab, +/-, and manual reload still work without
  UI advertising. Stale sensors/history-only mode remain explicit in the
  fixed-height footer. Preserve the terminal background.
- No async runtime, network listener, D-Bus dependency or external command per
  sample. Collection reads Linux power_supply uevent files directly.

## Boundaries

`battery.rs` handles hardware differences and unit normalization. `storage.rs`
handles transactional frames, schema versions, retention and read-only access.
`daemon.rs` schedules persistent recording. `monitor.rs` schedules live collection,
maintains a bounded ten-minute per-battery series, reads optional saved history,
and merges the two sources without duplicates. `usage.rs` integrates recorded
battery outflow into hourly buckets, independently of the live series and selected
graph window. `predict.rs` consumes measured history. `tui.rs` handles presentation,
colors, graphs and interaction.

```text
sysfs ── daemon / 30s ── SQLite ── read-only history ─┐
                                                   ├─ graph / predictor
sysfs ── monitor / 1s ── bounded in-memory series ────┘
```

The initial implementation coupled display freshness to the recorder's disk
interval. This was unsuitable for a live monitor: it could leave a transitional
zero on screen for 30 seconds and compressed short traces into a six-hour window.
Live sampling is now intentionally separate. Extra reads occur only while a
monitor is open; closing it returns to the recorder's low-frequency workload.
The one-second series is never silently written to the persistent training data.

The predictor is a separate function/module, not a plugin system. Inference runs
in the monitor for now, so recording continues without prediction overhead when
no monitor is open. Estimates use recent merged measurements independently of
the selected display window. Sensor failure disables ETA; unavailable saved
history alone does not disable an estimate from valid live samples. A future
heavier predictor can run elsewhere without changing the raw measurement schema.
Do not run neural-network training in the collection loop.

## Measurement semantics

A frame contains wall-clock time, Linux boot ID, CLOCK_BOOTTIME and
CLOCK_MONOTONIC readings, and the configured interval. These clocks allow
restarts, suspend intervals and wall-clock discontinuities to be distinguished.
Each frame has zero or more battery readings. A frame and all its readings are
committed together; zero batteries is different from an inactive recorder.

Device IDs initially use the kernel power_supply name (for example BAT0). Stable
identity across battery replacements is not implemented. Before long-term
battery-specific learning, add device identity/epoch handling without requiring
serial numbers to appear in the UI or logs.

Missing/invalid numeric fields remain null. Measured micro-units are normalized
to Wh, W, V, A and Ah. Charge-based energy uses design voltage when available,
otherwise current voltage, and is explicitly marked approximate. Raw charge
fields are retained. Power remains an unsigned magnitude in storage, the card
and graph. Charging/discharging status supplies a text label and historical trace
color instead of a sign. The generic Power heading is accompanied by a battery
source label: it is battery energy flow, not PC load or adapter input.
A zero POWER_NOW does not mask usable measured current × voltage, but a genuine
reported zero is retained, not substituted with an old rate. Firmware updates may
be slower than sensor reads. Scope=Device batteries are not system batteries.

Current is measured when CURRENT_NOW exists. In Details only, an absent current
can be derived as W / V from the same snapshot, with finite nonnegative watts and
finite positive volts. Measured current takes precedence, including zero. Derived
current is shown with `~` and two decimal places, or N/A when the inputs are
unusable. It never mutates BatteryReading, SQLite or the predictor inputs; raw
missing current remains missing. Stored energy in Wh alone does not determine
instantaneous current.

## Hourly battery use

The sidebar compares outgoing battery energy, not screen-on time or whole-PC use.
It shows 24 UTC hour buckets (23 previous hours and the current unfinished hour),
independently of the left graphs' window. UTC avoids ambiguous repeated local
hours and needs no timezone dependency. Direct AC consumption is not measurable
by this collector.

Only saved frames participate: the volatile live series must not double-count
recordings or make long-term totals depend on how long the viewer was open.
Trapezoidal integration of measured watts uses boottime elapsed duration, dividing
intervals proportionally at UTC wall-hour boundaries. Each pair must be continuous
and have the same known battery state. Charging/full/paused pairs contribute zero
outgoing energy; they never subtract from discharge. Discharging pairs require
finite nonnegative powers at both ends. Missing rates do not use energy-counter
fallback or forward-filled power. Missing-battery frames are preserved by a LEFT
JOIN, preventing integration across an empty heartbeat. State transitions,
suspend, boot changes, wall-clock jumps and large gaps are not integrated.
Overlapping wall times after a clock rollback are not counted twice.

Each bucket retains observed Wh and covered seconds. No coverage is unknown,
not zero; incomplete past hours are marked `~`, missing hours `·`, and the current
observed hour `*`. A fully covered zero-use hour gets `0`. `Observed` sums usable
intervals without extrapolation. Bars remain white regardless of live state.
A single frame cannot supply an interval, and absent/inaccessible recordings
never produce fabricated zero bars.

The reader streams only the selected battery's 24-hour window plus one preceding
frame through the accumulator, retaining 24 buckets and one previous sample,
not another day of raw history. Cache by last saved frame, battery and UTC hour;
changing the live graph window does not recompute the histogram. Refresh follows
the existing five-second history check; a stopped recorder still lets the window
advance hourly. Query failures are separate from live sensor and short-history
availability. No schema, daemon cadence or dependency change is needed.

## Batteryless power monitoring (not implemented)

A battery is one measurement source, not a requirement for measuring all kinds of
power. Future sources may include RAPL energy counters, driver-provided GPU/hwmon
power readings, or an external whole-system meter. The current battery collector
only records empty heartbeat frames when no system battery is present.

Keep source identity, domain and measurement scope explicit. Convert cumulative
energy to average watts using elapsed monotonic time, handle counter rollover and
reset/suspend discontinuities, and avoid counting overlapping RAPL domains twice.
Missing or inaccessible counters are unavailable, not zero. CPU/GPU component
measurements must never be relabeled as whole-PC consumption. Whole-system
readings require a sensor that actually covers that scope.

RAPL energy files can be root-readable only. Do not quietly run the monitor as
root, weaken sysfs permissions, or assume the current dedicated service user can
read them. Collection/access policy and a read-only transport for live readings
need a separate deployment decision. A batteryless UI should show available
power domains without meaningless battery charge, health or ETA panels.

## Persistence and permissions

SQLite schema version 1 has `frames` and `samples` tables. Opening a newer schema
fails instead of trying to modify it. Future schema upgrades must be explicit,
transactional and tested. History queries cover only the selected battery and
time range; graph points are reduced for drawing while preserving extrema.
The current implementation loads the selected raw range before reduction (up to
7 days); larger ranges or shorter sampling intervals may warrant SQL-side
aggregation after profiling.

Use rollback journaling in this initial version. WAL is attractive for reader /
writer concurrency, but direct read-only access from a group with no directory
write permission must also work when the daemon is stopped. Avoid granting write
access to readers merely to support SQLite sidecars. A future read-only IPC API
could revisit this choice if contention becomes measurable.

The writer holds an advisory lock for its lifetime. System data lives outside
the Nix store. Systemd creates `/var/lib/den` with owner/group den and mode 0750;
new DB/lock files are 0640. The monitor uses SQLite's read-only open flag with a short busy timeout so
saved-history contention does not stall live updates. Missing/unreadable history
is nonfatal in live mode; explicit `tui --history-only` mode requires it and never
reads sensors. Live errors are shown in the UI, while daemon collection errors go
to stderr/journald. Neither mode changes power settings or performs notification
or shutdown actions.

## Prediction roadmap

1. Establish interpretable baselines: remaining energy / smoothed discharge power,
   and unfilled reported capacity / smoothed charging power. Both require a
   continuous same-state history spanning at least 60 seconds. Charging currently
   assumes a constant future rate, so taper can make it optimistic; charge limits
   below full capacity are not modeled. Never display a discharge countdown while
   charging or treat design capacity as the target full capacity.
2. Collect representative histories, inspect quantization/missing data and build
   an offline evaluation dataset. Add export tooling only when its format is
   actually needed.
3. Compare simple statistical/regression approaches with the baseline.
4. Consider an optional local NN runtime and a separately distributed model only
   if evaluation demonstrates an improvement. Keep the ordinary package free of
   mandatory ML dependencies and keep a baseline fallback.

Potential features include recent power trends, battery capacity/health,
operating state and time of day. CPU load, brightness and power profiles are not
collected yet: add them only with a clear benefit and privacy/resource review.

Do not train using past predictions as ground truth. A discharge ending in
charging, shutdown or missing recording does not reveal its true time-to-empty;
it is an incomplete/censored observation, not an observed empty event. Define
the target (reported empty vs OS critical threshold, active runtime vs elapsed
wall time) before training or publishing accuracy claims.

Evaluation must split chronologically and by discharge session to avoid leakage.
Compare error in minutes and behavior under changing load with the baseline.
Display uncertainty bounds only after checking their coverage. Neither a model
nor a system service can know future user workload with certainty.

## Validation requiring a real deployment

Automated tests do not establish hardware compatibility or a resource budget.
Before enabling the service broadly, verify:

- Actual battery units, charge/discharge transitions and multiple batteries.
- Suspend/resume and restarts, including gaps in graphs and ETA warm-up.
- A non-root service can read the target machine's sysfs attributes under the
  configured systemd sandbox.
- A freshly logged-in reader can view history but cannot write DB/lock files,
  including after a clean stop and after service recovery from a crash.
- Idle CPU, memory, wakeups and DB write volume at the chosen interval.
- TUI appearance and keys in the user's terminal.

Automated live acceptance includes `python3 tests/live_pty.py`: the real monitor
is driven in a pseudo-terminal without a daemon or database, sysfs fixtures change
through an AC transition and a transient zero, and visible traces and subsequent
power updates must appear within seconds. Both states use unsigned watts;
historical traces retain their state colors while controls follow the current
state. Charging switches to a full-charge ETA card. There is no header row;
window selection belongs to the chart, and both graphs stay vertically stacked
with matching widths, including after a live terminal resize. The right-hand
Battery use / Details pair appears automatically on wide terminals and hides on
narrow ones until visibility is explicitly overridden. Both columns' borders must
align. Several hours of seeded saved data verify the integrated discharge total,
white bars, gap markers and independence from the live graphs' selected window. Help/Esc must not accidentally hide the automatic sidebar;
Esc closes manually opened Details rather than quitting. Primary and supporting
text retain their cool-white/blue-gray contrast even for idle readings; past power
traces keep green/orange state colors. The two-line Power card, finite W/V current
fallback, measured-current precedence, missing-voltage handling, compact help,
selector direction hints and Up/Down selection while expanded are exercised,
alongside sensor recovery, read-only behavior and terminal restoration.
This is not a substitute for physically unplugging/replugging the target laptop.

No system service is enabled by building/testing the project. A license must be
chosen before publishing/distributing the project under an explicit license.
