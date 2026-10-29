# User guide

[Back to the README](../README.md)

## Live monitoring and saved history

`den` and `den tui` open the same live monitor. It reads battery sensors every
second and retains up to ten minutes of high-resolution samples in memory. The
independent recorder normally saves every 30 seconds, with 90-day retention.
The viewer checks saved history every five seconds; live samples replace
overlapping saved samples without modifying the database.

The viewer never starts a recorder or creates a missing database. Missing,
inaccessible or busy history does not prevent live monitoring. Volatile samples
are lost when the viewer closes; run a recorder to retain history between sessions.

For manual recording, use separate terminals:

```sh
# Terminal 1
den --database "$HOME/den-history.sqlite3" daemon

# Terminal 2
den --database "$HOME/den-history.sqlite3"
```

For one snapshot or a custom cadence:

```sh
den daemon --once
den daemon --interval 60 --retention-days 30
```

A manually started recorder defaults to `$XDG_STATE_HOME/den/history.sqlite3`,
or `$HOME/.local/state/den/history.sqlite3`. The viewer prefers the system
history when present, otherwise the user-state location. Use the same explicit
`--database PATH` for both processes to avoid selecting different files.

`den tui --history-only` skips live sensors, requires readable history, and
anchors the graph and estimates at the last recording. The footer says `HISTORY`.

## Controls and layout

| Key | Action |
| --- | --- |
| `Left` / `Right` | Select 60s, 5m, 15m, 1h, 6h, 24h or 7d |
| `Up` / `Down` | Select Power / Charge without wrapping |
| `f` | Expand/restore the selected graph |
| `d` | Toggle the sidebar, or open Details on narrow terminals |
| `b` | Select the next battery, if there are several |
| `?` | Show controls and active diagnostics |
| `q`, `Esc`, `Ctrl-C` | Quit; Esc closes help or explicitly opened Details first |

`Tab` toggles graphs, `-` / `+` change the window, and `r` reloads saved history.
History refreshes automatically; normal viewing needs no manual reload.

Power is above Charge with aligned time axes. Both auto-scale. The Charge gauge
always represents 0–100%. Small terminals show only the selected graph. At
120+ columns and 24+ rows, Battery use and Details appear in a right sidebar.
Expanding a graph hides the automatic sidebar. An 80×24 terminal is recommended.

Charging is blue, Discharging copper orange, and reported Full green. Historical
segments keep their original state colors and break at transitions. Ordinary
numbers remain white; charge below 30% is yellow and below 15% is red. Stale
sensors are flagged and disable estimates. Recorder diagnostics appear in help.

## What the measurements mean

**Power is battery flow, not total PC consumption or AC adapter input.** Watts
are unsigned magnitudes; the state indicates whether energy enters or leaves
the battery. Firmware may update readings more slowly than den's one-second
polling, and can report zero during AC transitions or while charge-limited.

A genuine zero stays zero, not the previous non-zero reading. Positive measured
power is preferred; measured current × voltage can supply the rate when power
is missing or zero. Missing/unusable values remain `N/A`.

Details prefers measured current. When absent, usable watts / volts can supply a
display-only estimate marked `~`, such as `~0.58 A`. This never replaces raw saved
current. Wh alone cannot provide instantaneous current. Charge-only batteries
have approximate Wh conversions, also marked `~`; raw Ah values are retained.
Peripheral batteries with `Scope=Device` are excluded.

Graphs break at missing data, sensor failures, restarts, suspend, clock jumps and
large gaps rather than drawing invented continuity. A batteryless machine can
produce an empty heartbeat, not a fabricated zero-watt whole-PC reading.
CPU/GPU/whole-system collection is not implemented; see the [design](design.md).

## Hourly battery use

**Battery use · Wh** uses saved recordings only, independent of the graph window.
It covers the previous 23 UTC hours and the current unfinished hour. The final
bucket is not extrapolated to a full hour.

Continuous discharging watts are integrated over usable recorded intervals.
Charging is excluded, not subtracted from discharge. Missing readings, state
transitions, restarts, suspend and gaps do not invent energy or coverage.
Energy-counter changes are not used as a fallback in this chart.

`Observed` is the measured-interval sum, **not an extrapolated 24-hour total**.
Bars are white; labels are UTC. Markers mean:

- `·`: no usable interval, not zero usage.
- `~`: partially covered past hour.
- `*`: current hour with some usable recording, still in progress.
- `0`: fully covered past hour with zero outgoing battery energy.

No saved history or a single snapshot cannot establish hourly usage. AC-supplied
power that bypasses the battery is outside this measurement.

## Live power settings

Details reads kernel controls every five seconds without root, D-Bus or external
commands:

| Field | Meaning |
| --- | --- |
| Platform | Firmware `platform_profile`, not an inferred TLP/PPD selection |
| Driver | CPU frequency `scaling_driver` |
| EPP | CPU energy/performance preference |
| Pstate | Supported Intel/AMD pstate mode, such as active/passive |
| Governor | CPU frequency governor; shown when space permits |

These are separate settings, not one interchangeable profile. Different known
CPU policy values show `Mixed`; incomplete, inaccessible or unsupported controls
show `N/A`, never an old carried-forward setting. Long EPP/profile names are
abbreviated for display.

Settings are **live only**: not saved to SQLite or used by predictions.
History-only mode does not read current controls and says they were not recorded.
A custom `--sysfs` path ending in `class/power_supply` uses that tree's root for
controls; other fixture layouts do not fall through to host CPU settings.
Den changes none of these controls and does not start a power-management service.

## Experimental ETA

- Discharging: remaining energy / recent discharge power.
- Charging: (reported full energy − current energy) / recent charging power.
  The target is reported full capacity, not design capacity or a configured limit.
- Full / Not charging: the driver-reported state, without a countdown. A charge
  limit can normally stop charging below 100% with `Not charging`. This state
  does not identify the reason for stopping; den does not collect the limit.

At least three continuous readings spanning 60 seconds in the same state are
required. Compatible saved history can warm up the live estimator. State changes,
suspend and interrupted history reset it; stale sensors disable it. Directional
energy trends can provide a fallback when power is unavailable.

```text
In  15m – 40m
At  21:23 – 21:48
```

In is the duration range; At is the corresponding local 24-hour clock range.
Both use the selected reading's timestamp and the same displayed bounds,
including history-only mode. No day suffixes are added: In provides the elapsed
duration. UTC-offset transitions use explicitly labelled UTC times to avoid
ambiguous local clocks. Narrow layouts remove spaces/labels before values; an
unrepresentable clock range shows `N/A`. Open-ended durations have open-ended
clock ranges, not a fabricated upper endpoint.

The range uses time-weighted complete-minute rate percentiles with a minimum
±20% baseline rate margin, rounded outwards to five minutes. Slow scenarios over
seven days stay open-ended. See the [design](design.md#prediction-roadmap).

**This is an uncalibrated heuristic, not a probability/confidence interval.**
Accuracy and coverage are unvalidated; future workload can change. Charging
taper and charge limits are not modeled, so even the range can be optimistic.
Empty means reported zero energy, not an OS shutdown threshold. Estimates never
replace measured history or run in the recorder.

## Recorder operation and permissions

The [NixOS module](../README.md#nixos-service) runs a hardened system service as
`den`, independently of logins. If your separate NixOS module takes an `inputs`
argument, set `specialArgs = { inherit inputs; };` in the enclosing
`nixpkgs.lib.nixosSystem` call.

State is `/var/lib/den`, mode `0750`; new database
and lock files are mode `0640` under umask `0027`. Authorized readers join the
`den` group and must log in again. Only the recorder can write history. History
can reveal machine usage patterns; do not publish personal recordings.

```sh
systemctl status den
journalctl -u den
```

SQLite stores transactional snapshots, including empty heartbeat frames, with
one writer enforced by an advisory lock. Retention runs at startup and roughly
hourly; deleting old rows need not shrink the file immediately. Rollback
journaling lets read-only viewers work without writable WAL sidecars. After an
unclean shutdown, the recorder may need to recover SQLite before viewing works.

Collection failures are logged and retried at the next interval; `--once` instead
returns failure. Suspend intervals are not sampled and no wake timers are added.
Automatic non-NixOS service setup, Home Manager integration and a user-service
unit are not provided. Hardware behavior, actual service permissions/sandboxing
and resource consumption still require real deployment validation.
