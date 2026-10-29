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
- Headerless UI: four summary cards (power, charge, ETA, health), each with two
  content lines and four rows including borders. Charge combines percentage and
  remaining Wh above its gauge; normal-width summaries save one row. Narrow
  layouts use two rows of cards. Branding stays at the footer's right edge.
  The Power card has two content lines: accented state
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
  not-charging readings, brighter blue-gray secondary information/axis labels, blue-gray
  borders, and white footer keys instead of dimming all supporting text. Blue
  Charging (#8FB5E8), copper orange Discharging (#D18B47) and green Full (#8BC592)
  apply to selection, focus, state labels, both historical traces/endpoints and
  the Charge gauge. The ETA's Full status is green; ordinary numeric values stay
  white.
  Remaining-level color bands are removed; only the Charge number retains low-level
  warnings (red <15%, yellow <30%). Both histories break at state changes and
  preserve original state colors instead of repainting the past. Both graphs
  auto-scale without a redundant label on Charge. Hourly battery-use bars stay
  cool white independently of the live state. Historical state colors and low-charge
  warnings are independent of the current accent. The
  footer only advertises window, graph selection, help and quit, not every optional
  action. Up selects Power and Down selects Charge without wrapping, including in
  expanded mode. Normal help is a content-sized key table; battery selection
  appears only for multiple batteries, diagnostics only on errors. Routine
  recording cadence and save-age are removed from Details; stale-recorder warnings
  are shown only in help and only when a live recorder is expected. Explanatory
  prose belongs in README. Details also shows live firmware/CPU power settings;
  compact panels prioritize Platform, Driver, EPP and Pstate over Governor.
  Legacy Tab, +/-, and manual reload still work without
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
graph window. `power.rs` reads live platform/CPU controls on a separate five-second
cadence; these are not persisted or predictor inputs yet. `predict.rs` consumes
measured battery history. `tui.rs` handles presentation,
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

## Live power settings

Read firmware `platform_profile`, each CPU policy's driver/governor/EPP, and the
matching Intel/AMD pstate status without external commands, D-Bus, privilege
changes or writes. Cache for five seconds independently of database availability.
A refresh replaces missing/error values with unknown, not a stale prior setting.
Across policies, different known values are `Mixed`; any unavailable policy value
makes that field unknown. Do not infer a power-management provider or TLP/PPD
selection from AC state, governor or firmware profile. Keep platform, driver,
pstate mode, governor and EPP distinct.

Only standard .../class/power_supply paths imply a matching system sysfs root;
other custom fixture layouts must never fall through to the host's CPU settings.
History-only mode skips power-setting collection and labels it unrecorded. These
are live details only: preserving system context for historical prediction needs
a separate, explicit schema evolution with provenance/policy identity. Do not
attach the viewer's current system settings to older battery recordings.

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
and have the same known battery state. Charging/full/not-charging pairs contribute zero
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
2. Trial a recent-use scenario band without claiming calibrated probability:
   integrate measured watts into complete time-weighted 60-second means, use
   their 10th/90th percentiles, and widen to at least ±20% of the baseline rate.
   Divide the energy target by fast/slow rates; round time bounds outwards to five
   minutes. ETA pairs white/bold In (duration) with blue-gray At (projected clock
   times). Derive both from the same displayed bounds and selected snapshot's
   timestamp, including history-only mode; never add a historical duration to the
   viewer's current time. Coarser hour bounds on tight layouts must also govern
   At. Use local 24-hour clocks without day suffixes: In already conveys the
   elapsed duration, including midnight and multi-day ranges. Omit labels/spaces
   before dropping any values; an unrepresentable clock range is N/A, not clipped.
   UTC-offset transitions use explicitly labelled UTC times instead of ambiguous
   local intervals. Use libc's reentrant localtime/gmtime interfaces, without
   subprocesses or a timezone database dependency; libc was already transitive
   and is now a direct dependency for these necessary Linux interfaces. In/At
   calculation lives in eta.rs; neither daemon nor raw storage changes. Existing
   warm-up/unavailability notes remain, Full stays green and Not charging stays
   white without projected times. Do not alias Not charging as Paused: a normal
   charge-limit stop is not necessarily a user pause. Neither Full nor reaching a
   charge limit can be inferred from this state alone; limits are not collected. A one-row empirical histogram
   was trialled and removed because sparse floating blocks without axes were not
   self-explanatory. Do not restore a miniature distribution or fabricate a normal
   curve from the endpoints; a useful distribution view needs more room and clear
   axes. Removing the diagram does not change the numerical range algorithm.
   Energy-trend fallback uses the rate margin alone. Slow scenarios over
   seven days remain open-ended. This heuristic is implemented, not validated;
   charging taper, future load changes and charge limits remain outside the model.
3. Collect representative histories, inspect quantization/missing data and build
   an offline evaluation dataset. Add export tooling only when its format is
   actually needed.
4. Compare simple statistical/regression approaches with the baseline and evaluate
   interval coverage before making probability claims. Point-accuracy work is deferred.
5. Consider an optional local NN runtime and a separately distributed model only
   if evaluation demonstrates an improvement. Keep the ordinary package free of
   mandatory ML dependencies and keep a baseline fallback.

Potential features include recent power trends, battery capacity/health,
operating state and time of day. Power settings are live display only, not saved
features. CPU load and brightness are not collected: add them only with a clear
benefit and privacy/resource review.

Do not train using past predictions as ground truth. A discharge ending in
charging, shutdown or missing recording does not reveal its true time-to-empty;
it is an incomplete/censored observation, not an observed empty event. Define
the target (reported empty vs OS critical threshold, active runtime vs elapsed
wall time) before training or publishing accuracy claims.

Evaluation must split chronologically and by discharge session to avoid leakage.
Compare error in minutes and behavior under changing load with the baseline.
Do not label the trial scenario band a confidence/probability interval until
empirical coverage has been checked. Neither a model nor a system service can
know future user workload with certainty.

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
text retain their cool-white/blue-gray contrast even for idle readings. Charge
history and the summary gauge follow charging/discharging state, not remaining
level. Tests cover historical state transitions, invalid/missing levels, no past
repainting, two-line cards, live power-setting refresh, heuristic ETA ranges,
white duration ranges paired with secondary local clock ranges, and green Full
status. UTC-controlled unit tests cover shared bounds/reference time, midnight,
year/leap-day transitions, UTC-offset changes, overflow, open ranges and narrow
layouts; the real PTY runs in a fixed non-UTC zone to check local conversion. The compact ETA card must not contain
ambiguous histogram blocks.
The two-line Power card, finite W/V current
fallback, measured-current precedence, missing-voltage handling, compact help,
selector direction hints and Up/Down selection while expanded are exercised,
alongside sensor recovery, read-only behavior and terminal restoration.
This is not a substitute for physically unplugging/replugging the target laptop.

No system service is enabled by building/testing the project. A license must be
chosen before publishing/distributing the project under an explicit license.
