#!/usr/bin/env python3
"""Exercise the real TUI and changing sysfs fixtures in an isolated pseudo-terminal.

Usage: python3 tests/live_pty.py [target/debug/den]
No daemon, physical power changes, or desktop terminal access is required.
"""

import codecs
from datetime import datetime, timedelta, timezone
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time

TEXT = (220, 228, 240)
SECONDARY = (186, 198, 216)
MUTED = (135, 147, 170)
ORANGE = (209, 139, 71)
BLUE = (143, 181, 232)
GREEN = (139, 197, 146)
YELLOW = (238, 199, 113)
RED = (240, 133, 148)
CSI = re.compile(r"\x1b\[([0-?]*)([ -/]*)([@-~])")


class Screen:
    def __init__(self, width, height):
        self.width, self.height = width, height
        self.cells = [[(" ", None, None) for _ in range(width)] for _ in range(height)]
        self.row = self.col = 0
        self.fg = self.bg = None
        self.pending = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        index = 0
        while index < len(self.pending):
            char = self.pending[index]
            if char == "\x1b":
                match = CSI.match(self.pending, index)
                if match is None:
                    break
                self.control(match.group(1), match.group(3))
                index = match.end()
                continue
            if char == "\r":
                self.col = 0
            elif char == "\n":
                self.row = min(self.row + 1, self.height - 1)
            elif char >= " ":
                if self.row < self.height and self.col < self.width:
                    self.cells[self.row][self.col] = (char, self.fg, self.bg)
                self.col += 1
            index += 1
        self.pending = self.pending[index:]

    def control(self, raw, command):
        private = raw.startswith("?")
        values = [int(value) if value else 0 for value in raw.lstrip("?").split(";")]
        if command in ("H", "f"):
            self.row = max(0, (values[0] or 1) - 1)
            self.col = max(0, (values[1] if len(values) > 1 else 1) - 1)
        elif command == "J" and values[0] in (2, 3):
            self.cells = [[(" ", None, None) for _ in range(self.width)] for _ in range(self.height)]
        elif command == "K":
            start = 0 if values[0] in (1, 2) else self.col
            end = self.width if values[0] in (0, 2) else self.col + 1
            if self.row < self.height:
                for col in range(start, end):
                    self.cells[self.row][col] = (" ", self.fg, self.bg)
        elif command == "m":
            index = 0
            while index < len(values):
                value = values[index]
                if value == 0:
                    self.fg = self.bg = None
                elif value == 39:
                    self.fg = None
                elif value == 49:
                    self.bg = None
                elif value in (38, 48) and values[index + 1:index + 2] == [2]:
                    color = tuple(values[index + 2:index + 5])
                    if value == 38:
                        self.fg = color
                    else:
                        self.bg = color
                    index += 4
                elif value == 30:
                    self.fg = (0, 0, 0)
                index += 1
        elif command == "h" and private and 1049 in values:
            self.row = self.col = 0

    def text(self):
        return "\n".join("".join(char for char, _, _ in row) for row in self.cells)

    def text_color(self, text):
        for row in self.cells:
            line = "".join(char for char, _, _ in row)
            col = line.find(text)
            if col >= 0:
                return row[col][1]
        raise AssertionError(f"Text not found: {text}")

    def trace_cells(self, color, graph=None):
        rows = self.cells
        if graph:
            lines = self.text().splitlines()
            top = next(index for index, line in enumerate(lines) if f"{graph} · " in line)
            bottom = next(index for index in range(top + 1, self.height) if lines[index].startswith("╰"))
            rows = rows[top:bottom + 1]
        return sum(fg == color and "\u2801" <= char <= "\u28ff"
                   for row in rows for char, fg, _ in row)


def main():
    project = Path(__file__).resolve().parent.parent
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else project / "target/debug/den").resolve()
    target = project / "target"
    target.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="den-live-test-", dir=target) as temporary:
        directory = Path(temporary)
        sysfs = directory / "sys"
        supplies = sysfs / "class/power_supply"
        for name in ("AC", "BAT0"):
            (supplies / name).mkdir(parents=True)
        for path, value in {
            "firmware/acpi/platform_profile": "balanced",
            "devices/system/cpu/intel_pstate/status": "active",
            "devices/system/cpu/cpufreq/policy0/scaling_driver": "intel_pstate",
            "devices/system/cpu/cpufreq/policy0/scaling_governor": "powersave",
            "devices/system/cpu/cpufreq/policy0/energy_performance_preference": "balance_performance",
        }.items():
            setting = sysfs / path
            setting.parent.mkdir(parents=True, exist_ok=True)
            setting.write_text(value + "\n")

        def fixture(status, watts, percent=52, voltage=12.5, current=None):
            values = {
                "AC": f"POWER_SUPPLY_TYPE=Mains\nPOWER_SUPPLY_ONLINE={int(status in ('Charging', 'Full', 'Not charging'))}\n",
                "BAT0": (
                    f"POWER_SUPPLY_TYPE=Battery\nPOWER_SUPPLY_STATUS={status}\n"
                    f"POWER_SUPPLY_CAPACITY={percent}\nPOWER_SUPPLY_POWER_NOW={round(watts * 1_000_000)}\n"
                    f"POWER_SUPPLY_ENERGY_NOW={50_000_000 if status == 'Full' else 26_000_000}\n"
                    "POWER_SUPPLY_ENERGY_FULL=50000000\n"
                    "POWER_SUPPLY_ENERGY_FULL_DESIGN=55000000\n"
                ),
            }
            if voltage is not None:
                values["BAT0"] += f"POWER_SUPPLY_VOLTAGE_NOW={round(voltage * 1_000_000)}\n"
            if current is not None:
                values["BAT0"] += f"POWER_SUPPLY_CURRENT_NOW={round(current * 1_000_000)}\n"
            for name, content in values.items():
                staged = supplies / name / "uevent.new"
                staged.write_text(content)
                os.replace(staged, supplies / name / "uevent")

        fixture("Discharging", 8.5)
        database = directory / "not-created.sqlite3"
        master, slave = pty.openpty()
        original = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 100, 0, 0))
        process = subprocess.Popen(
            [binary, "--database", database, "--sysfs", supplies],
            stdin=slave, stdout=slave, stderr=slave,
            env={**os.environ, "TERM": "xterm-256color", "TZ": "JST-9"},
        )
        screen = Screen(100, 32)
        output = bytearray()
        diagnostic = "den-live"
        local_zone = timezone(timedelta(hours=9))

        def wait_for(predicate, description, timeout=3):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                readable, _, _ = select.select([master], [], [], 0.05)
                if readable:
                    data = os.read(master, 65536)
                    output.extend(data)
                    screen.feed(data)
                if predicate():
                    # A frame may span several PTY writes. Drain the remaining
                    # paint before inspecting colors or saving a screenshot.
                    while select.select([master], [], [], 0.03)[0]:
                        data = os.read(master, 65536)
                        output.extend(data)
                        screen.feed(data)
                    if predicate():
                        return
            raise AssertionError(f"{description}\n{screen.text()}")

        def key(value, predicate, description):
            os.write(master, value)
            wait_for(predicate, description)

        def assert_stacked(details=None):
            if details is None:
                details = screen.width >= 120 and screen.height >= 24
            lines = screen.text().splitlines()
            power = next(index for index, line in enumerate(lines) if "Power · W" in line)
            charge = next(index for index, line in enumerate(lines) if "Charge · %" in line)
            assert charge > power, "charge graph must remain below power"
            sidebar = lines[0].rfind("╭")
            right = sidebar - 2 if details else screen.width - 1
            for index in (power, charge):
                assert lines[index][0] == "╭" and lines[index][right] == "╮", "graphs must have matching widths"
            assert ("55.0 Wh" in screen.text()) == details, "Details must be automatic on wide terminals, hidden on narrow ones"
            assert ("Battery use · Wh" in screen.text()) == details
            if details:
                assert "Battery use · Wh" in lines[power] and "Details" in lines[charge]
                assert lines[power][sidebar] == lines[charge][sidebar] == "╭"
                assert lines[charge - 2][sidebar] == lines[charge - 2][0] == "╰", "upper panel bottoms must align"
                assert lines[-3][sidebar] == lines[-3][0] == "╰", "lower panel bottoms must align"
                assert 9 <= screen.height - 2 - charge <= 12, "Details must retain a compact height"
            assert "Details · d / Esc" not in screen.text(), "no details popup should remain open"

        def assert_eta_range():
            title = screen.text().splitlines()[0]
            left = title.index("╭ Time to full")
            right = title.index("╮", left)
            numbers = "".join(char for char, _, _ in screen.cells[1][left + 1:right])
            assert numbers.strip() == "In  1h 05m – 1h 40m", "In must show the duration with readable spacing"
            arrival = "".join(char for char, _, _ in screen.cells[2][left + 1:right]).strip()
            assert arrival.startswith("At  ")
            assert screen.text_color(arrival) == SECONDARY
            normalized = arrival[4:].replace(" ", "")
            expected = set()
            now = int(time.time())
            for base in range(now - 3, now + 1):
                reference = datetime.fromtimestamp(base, local_zone)
                low = reference + timedelta(minutes=65)
                high = reference + timedelta(minutes=100)
                expected.add(f"{low:%H:%M}–{high:%H:%M}")
            assert normalized in expected, f"At must use the same bounds in local time, not UTC: {arrival}"
            assert screen.text_color("1h 05m – 1h 40m") == TEXT

        try:
            wait_for(lambda: all(text in screen.text() for text in (
                "Discharging", "8.5 W", "Time to empty", "Power · W", "Charge · %", "Quit"
            )), "initial live reading and both graphs")
            wait_for(lambda: screen.trace_cells(ORANGE, "Power") >= 3 and screen.trace_cells(ORANGE, "Charge") >= 3,
                     "visible live traces within seconds", timeout=5)
            assert all(text not in screen.text() for text in ("-8.5 W", "+8.5 W", "Battery power"))
            assert screen.text_color("8.5 W") == TEXT
            assert screen.text_color("Discharging") == ORANGE
            assert screen.text_color("Battery · BAT0") == SECONDARY
            assert screen.text_color("←→") == TEXT
            assert screen.text_color("↑↓") == TEXT
            assert "Discharging 8.5 W" in screen.text().splitlines()[1]
            assert "Battery · BAT0" in screen.text().splitlines()[2]
            assert "Focus" not in screen.text().splitlines()[-1]
            assert_stacked()
            (target / "den-live-discharging.ansi").write_bytes(output)
            assert "Battery health" in screen.text().splitlines()[0], "there must not be a header row"
            selector = screen.text().splitlines()[5]
            assert all(text in selector for text in ("←", "60s", "7d", "→")), "selector must show arrow keys"
            assert screen.cells[5][selector.index("←")][1] == MUTED
            assert screen.cells[5][selector.index("→")][1] == TEXT
            assert screen.text().splitlines()[-1].endswith("den"), "brand belongs in the footer"
            charge_x = screen.text().splitlines()[0].index("╭ Charge") + 1
            for percent in [95, 60, 20, 10, 52]:
                fixture("Discharging", 8.5, percent)
                wait_for(lambda: f"{percent:.1f}%" in screen.text() and screen.cells[2][charge_x][1] == ORANGE,
                         "Charge gauge follows power state, not remaining level")
                assert screen.text_color("Discharging") == ORANGE
                assert screen.cells[5][0][1] == ORANGE
                if percent == 95:
                    (target / "den-state-colors-test.ansi").write_bytes(output)
            fixture("Charging", 0.0)
            wait_for(lambda: "0.0 W" in screen.text() and "Charging" in screen.text()
                     and screen.cells[5][0][1] == BLUE, "AC transition zero and state accent")
            assert screen.cells[2][charge_x][1] == BLUE
            assert screen.text_color("0.0 W") == TEXT
            fixture("Charging", 32.4, 53)
            wait_for(lambda: all(text in screen.text() for text in ("32.4 W", "53.0%", "Time to full")),
                     "unsigned charging flow and full-charge estimate after replug")
            fixture("Charging", 18.2, 53)
            wait_for(lambda: "18.2 W" in screen.text() and screen.trace_cells(BLUE, "Power") >= 2, "charging power trace")
            assert all(text not in screen.text() for text in ("+18.2 W", "-18.2 W"))
            assert screen.trace_cells(ORANGE, "Power") >= 3, "past discharge trace must keep its own color"
            assert screen.text_color("18.2 W") == TEXT
            assert screen.text_color("Charging") == BLUE
            assert screen.text().count("Charging") == 1, "battery status must not be repeated"
            assert all(text not in screen.text() for text in ("LIVE 1s", "UTC", "Not discharging")), "cluttered header"
            assert not database.exists(), "the viewer must not create a history database"
            offline = directory / "offline"
            supplies.rename(offline)
            wait_for(lambda: "SENSOR STALE" in screen.text(), "sensor error is visible")
            offline.rename(supplies)
            wait_for(lambda: "SENSOR STALE" not in screen.text() and "18.2 W" in screen.text()
                     and screen.cells[5][0][1] == BLUE, "sensor error recovery")
            key(b"\x1b[B", lambda: any("Charge · %" in line and screen.cells[index][0][1] == BLUE
                for index, line in enumerate(screen.text().splitlines())), "Down selects the lower graph")
            os.write(master, b"\x1b[B")  # Repeating Down must not wrap back to Power.
            key(b"f", lambda: "Charge · %" in screen.text().splitlines()[5]
                and "7d" in screen.text().splitlines()[5] and "Power · W" not in screen.text(),
                "expand charge graph with its window selector")
            key(b"\x1b[A", lambda: "Power · W" in screen.text() and "Charge · %" not in screen.text(),
                "Up selects Power while expanded")
            key(b"\x1b[B", lambda: "Charge · %" in screen.text() and "Power · W" not in screen.text(),
                "Down selects Charge while expanded")
            key(b"f", lambda: "Charge · %" in screen.text() and "Power · W" in screen.text(), "restore both graphs")
            key(b"\x1b[C", lambda: "5m ago" in screen.text(), "change the shared graph window")
            selector = screen.text().splitlines()[5]
            assert screen.cells[5][selector.index("←")][1] == TEXT
            key(b"\x1b[D", lambda: "60s ago" in screen.text() and "5m ago" not in screen.text(), "restore window")
            fixture("Not charging", 0.0)
            wait_for(lambda: "Not charging" in screen.text() and "0.0 W" in screen.text(), "idle readings stay visible")
            assert "Paused" not in screen.text()
            assert screen.text_color("0.0 W") == TEXT
            # Not charging appears in both cards; only the ETA value is white.
            eta_title = screen.text().splitlines()[0]
            eta_left = eta_title.index("╭ Time to full")
            eta_right = eta_title.index("╮", eta_left)
            eta_value = "".join(char for char, _, _ in screen.cells[1][eta_left + 1:eta_right])
            assert eta_value.strip() == "Not charging"
            assert screen.cells[1][eta_left + 1][1] == TEXT
            assert screen.text_color("Not charging") == SECONDARY
            fixture("Full", 0.0, 100)
            eta_x = screen.text().splitlines()[0].index("╭ Time to full") + 1
            wait_for(lambda: screen.cells[1][eta_x][0:2] == ("F", GREEN)
                     and screen.cells[2][charge_x][1] == GREEN, "Full status and gauge are green, distinct from blue charging")
            assert screen.text_color("0.0 W") == TEXT
            (target / "den-full-cells.json").write_text(json.dumps(screen.cells))
            (target / "den-full-screen.txt").write_text(screen.text())
            fixture("Charging", 18.2, 53)
            wait_for(lambda: "18.2 W" in screen.text() and "Charging" in screen.text()
                     and screen.text_color("Charging") == BLUE, "restore charging accent")
            key(b"d", lambda: "Details · d / Esc to close" in screen.text(), "open narrow-terminal details")
            assert "Current  ~1.46 A" in screen.text(), "missing current should be derived from W / V"
            assert all(text in screen.text() for text in ("Platform balanced", "Driver   intel_pstate", "EPP      balance perf.", "Governor powersave", "Pstate   active"))
            epp = sysfs / "devices/system/cpu/cpufreq/policy0/energy_performance_preference"
            epp.write_text("power\n")
            wait_for(lambda: "EPP      power" in screen.text(), "power settings refresh independently of the database", timeout=6)
            fixture("Charging", 18.2, 53, current=0.42)
            wait_for(lambda: "Current  0.42 A" in screen.text(), "measured current takes precedence")
            assert "~0.42 A" not in screen.text()
            fixture("Charging", 18.2, 53, voltage=None)
            wait_for(lambda: "Current  N/A" in screen.text(), "no current is invented without voltage")
            fixture("Charging", 0.0, 53)
            wait_for(lambda: "Current  ~0.00 A" in screen.text(), "derived zero remains a valid reading")
            fixture("Charging", 18.2, 53)
            wait_for(lambda: "Current  ~1.46 A" in screen.text(), "restore derived current")
            key(b"\x1b", lambda: "Details · d / Esc to close" not in screen.text(), "close narrow-terminal details")
            assert process.poll() is None
            key(b"?", lambda: "Select graph" in screen.text() and "Expand / restore" in screen.text(), "short help with optional-history diagnostics")
            assert "Sensors are read" not in screen.text()
            assert "Next battery" not in screen.text()
            key(b"\x1b", lambda: "Help · Esc to close" not in screen.text(), "close help")
            key(b"q", lambda: process.poll() is not None and b"\x1b[?1049l" in output, "quit and leave alternate screen")
            assert process.wait(timeout=3) == 0
            assert termios.tcgetattr(slave) == original, "terminal settings were not restored"
            assert b"\x1b[?1049l" in output, "alternate screen was not left"
            (target / "den-live-screen.txt").write_text(screen.text())
            (target / "den-live-test.ansi").write_bytes(output)

            # Seed compatible earlier frames using the recorder's own clocks and
            # schema, so the full-charge countdown is tested without a 60s sleep.
            database = directory / "charging.sqlite3"
            subprocess.run(
                [binary, "--database", database, "--sysfs", supplies, "daemon", "--once"],
                check=True, capture_output=True,
            )
            with sqlite3.connect(database) as connection:
                origin = connection.execute(
                    "SELECT timestamp, boot_id, boottime_ms, monotonic_ms FROM frames WHERE id = 1"
                ).fetchone()
                columns = [row[1] for row in connection.execute("PRAGMA table_info(samples)")
                           if row[1] != "frame_id"]
                baseline = dict(zip(columns, connection.execute(
                    f"SELECT {', '.join(columns)} FROM samples WHERE frame_id = 1"
                ).fetchone()))
                connection.execute("DELETE FROM samples")
                connection.execute("DELETE FROM frames")
                hour = origin[0] // 3600 * 3600
                recordings = []
                for index, watts in enumerate((4.0, 7.0, 12.0, 0.0, 9.0, 15.0, 6.0)):
                    if index == 1:
                        continue  # An unrecorded hour must not become a zero bar.
                    start = hour - (7 - index) * 3600
                    for second in range(0, 3600, 30):
                        if index == 5 and 300 <= second < 600:
                            continue  # A partially recorded hour gets a gap marker.
                        recordings.append((start + second, "Discharging", watts))
                for timestamp in range(hour, origin[0] - 390, 30):
                    recordings.append((timestamp, "Discharging", 8.5))
                recordings = [row for row in recordings if row[0] < origin[0] - 390]
                # Measured load variation exercises the range without a long sleep.
                rates = [16.0, 20.0, 17.0, 20.0, 18.2, 18.2]
                recordings += [(origin[0] + elapsed, "Charging", rates[(elapsed + 360) // 60])
                               for elapsed in range(-360, 0, 30)]
                for timestamp, status, watts in recordings:
                    elapsed = timestamp - origin[0]
                    frame_id = connection.execute(
                        "INSERT INTO frames(timestamp, boot_id, boottime_ms, monotonic_ms, interval_secs) "
                        "VALUES (?, ?, ?, ?, 30)",
                        (timestamp, origin[1], origin[2] + elapsed * 1000, origin[3] + elapsed * 1000)
                    ).lastrowid
                    reading = {**baseline, "status": status, "power_w": watts,
                               "external_power": int(status == "Charging")}
                    connection.execute(
                        f"INSERT INTO samples(frame_id, {', '.join(columns)}) "
                        f"VALUES ({', '.join('?' for _ in range(len(columns) + 1))})",
                        [frame_id] + [reading[column] for column in columns],
                    )
                used_wh = sum((first[2] + last[2]) / 2 * (last[0] - first[0]) / 3600
                              for first, last in zip(recordings, recordings[1:])
                              if first[1] == last[1] == "Discharging" and 0 < last[0] - first[0] <= 90)
                observed = f"Observed {used_wh:.1f} Wh"

            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
            screen = Screen(140, 40)
            output = bytearray()
            diagnostic = "den-charge"
            process = subprocess.Popen(
                [binary, "--database", database, "--sysfs", supplies],
                stdin=slave, stdout=slave, stderr=slave,
                env={**os.environ, "TERM": "xterm-256color", "TZ": "JST-9"},
            )
            wait_for(lambda: all(text in screen.text() for text in (
                "18.2 W", "Time to full", "In  1h 05m – 1h 40m", "Charge · %"
            )), "full-charge scenario range from recorded and live data")
            assert_eta_range()
            assert_stacked()
            assert observed in screen.text(), "hourly usage must integrate saved discharge, not live charging"
            assert "24h · hourly UTC" in screen.text() and "~ gaps" in screen.text()
            lines = screen.text().splitlines()
            sidebar = lines[0].rfind("╭")
            charge = next(index for index, line in enumerate(lines) if "Charge · %" in line)
            bars = [fg for row in screen.cells[10:charge - 4] for char, fg, _ in row[sidebar + 1:]
                    if char in "▁▂▃▄▅▆▇█"]
            assert bars and all(color == TEXT for color in bars), "historical usage bars must remain white"
            key(b"\x1b[C", lambda: "5m ago" in screen.text(), "change left windows independently of hourly usage")
            assert observed in screen.text()
            key(b"\x1b[D", lambda: "60s ago" in screen.text(), "restore the live graph window")
            key(b"f", lambda: "Battery use · Wh" not in screen.text() and "Charge · %" not in screen.text(), "expanded power uses the full width")
            assert screen.text().splitlines()[5][-1] == "╮"
            key(b"f", lambda: "Battery use · Wh" in screen.text() and "Charge · %" in screen.text(), "restore aligned two-column panels")
            assert_stacked()
            top = screen.text().splitlines()[0]
            assert top.index("Power") < top.index("Charge") < top.index("Time to full") < top.index("Battery health")
            assert "Recent use" not in screen.text() and "Recent charge" not in screen.text()
            assert all(text not in screen.text() for text in ("auto scale", "Recording", "Every 30s", "Last saved"))
            assert screen.text_color("18.2 W") == screen.text_color("1h 05m – 1h 40m") == TEXT
            assert screen.text().splitlines()[3].startswith("╰"), "all cards must be four rows tall"
            assert screen.cells[-1][-3][1] == TEXT, "brand must remain bright"
            assert screen.text_color("55.0 Wh") == TEXT
            key(b"?", lambda: "Select graph" in screen.text() and "Toggle sidebar" in screen.text(), "compact help on a wide terminal")
            help_row = next(index for index, line in enumerate(screen.text().splitlines()) if "Help · Esc" in line)
            left = screen.text().splitlines()[help_row].index("╭")
            assert screen.cells[help_row + 6][left][0] == "╰", "healthy help should be seven rows tall"
            assert all(text not in screen.text() for text in ("Sensors are read", "Prediction:", "History:", "Next battery"))
            (target / "den-help-screen.txt").write_text(screen.text())
            (target / "den-help-test.ansi").write_bytes(output)
            key(b"\x1b", lambda: "Help · Esc to close" not in screen.text(), "close help without hiding automatic Details")
            assert_stacked()
            for width, height in [(200, 50), (100, 32), (200, 50)]:
                screen = Screen(width, height)
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
                process.send_signal(signal.SIGWINCH)
                wait_for(lambda: "Power · W" in screen.text() and "Charge · %" in screen.text()
                         and screen.text().splitlines()[-1].endswith("den"), "resize and automatically show/hide Details")
                assert_stacked()
                assert_eta_range()
            key(b"d", lambda: "55.0 Wh" not in screen.text(), "hide wide-terminal Details")
            assert_stacked(details=False)
            key(b"d", lambda: "55.0 Wh" in screen.text(), "restore wide-terminal Details")
            assert_stacked()
            key(b"\x1b", lambda: "55.0 Wh" not in screen.text(), "Esc closes explicitly opened Details, not the monitor")
            assert process.poll() is None
            key(b"d", lambda: "55.0 Wh" in screen.text(), "leave Details visible")
            assert_stacked()
            key(b"q", lambda: process.poll() is not None and b"\x1b[?1049l" in output, "quit charging preview")
            assert process.wait(timeout=3) == 0
            assert termios.tcgetattr(slave) == original
            with sqlite3.connect(database) as connection:
                assert connection.execute("SELECT count(*) FROM frames").fetchone()[0] == len(recordings)
                assert connection.execute("SELECT count(*) FROM samples").fetchone()[0] == len(recordings)
                assert connection.execute("SELECT count(*) FROM samples WHERE current_a IS NOT NULL").fetchone()[0] == 0
            print("PASS: two-row Power/Charge/ETA/health cards with In/At ETA ranges, local-time projection and no ambiguous histograms, blue charging/copper discharging/green Full, live power settings, heuristic ETA ranges, hourly saved usage, aligned layout and resize, current fallback, help, arrow keys, sensor recovery, read-only history, terminal restoration")
        finally:
            (target / f"{diagnostic}-screen.txt").write_text(screen.text())
            (target / f"{diagnostic}-cells.json").write_text(json.dumps(screen.cells))
            (target / f"{diagnostic}-test.ansi").write_bytes(output)
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=3)
            os.close(master)
            os.close(slave)


if __name__ == "__main__":
    main()
