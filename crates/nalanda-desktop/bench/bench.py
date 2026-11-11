#!/usr/bin/env python3
"""Paired benchmark of two Nalanda desktop builds on sway (Wayland).

For each round, alternating the order, each app is started on the same library and measured:
  map_ms     spawn -> sway "window new" event for its pid (a Wayland toplevel maps on its
             first committed buffer, so this is time to first frame on screen)
  rss_mb     VmRSS 4 s after mapping (idle, first screen of covers decoded)
  idle_cpu   CPU ms used (all threads, user+sys) over 10 s untouched
  scroll_cpu CPU ms used while a virtual pointer (zwlr_virtual_pointer_v1) sends wheel notches
             over the grid, spread over 4 s, plus 1.5 s to settle. Each app gets the notch count
             that moves it the same distance (they scroll different amounts per notch).
  rss_after  VmRSS after the scroll (covers for every row passed were decoded)

usage: bench.py ROUNDS NAME=NOTCHES=CMD [...]  (run inside the headless sway: source nested.sh)   (env NALANDA_LIBRARY picks the library)
"""
import json, os, subprocess, sys, time, threading, statistics
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vpointer import VPointer

TICK = os.sysconf("SC_CLK_TCK")


def cpu_ms(pid):
    with open(f"/proc/{pid}/stat") as f:
        fields = f.read().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) * 1000 / TICK


def rss_mb(pid):
    with open(f"/proc/{pid}/status") as f:
        for line in f:
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) / 1024


def sway(*cmd):
    return subprocess.run(["swaymsg", *cmd], capture_output=True, text=True).stdout


class Watcher:
    """Collects sway window events with their arrival time."""

    def __init__(self):
        self.events = []
        self.proc = subprocess.Popen(
            ["swaymsg", "-t", "subscribe", "-m", '["window"]'],
            stdout=subprocess.PIPE, text=True)
        threading.Thread(target=self._read, daemon=True).start()
        time.sleep(0.3)

    def _read(self):
        for line in self.proc.stdout:
            try:
                ev = json.loads(line)
            except ValueError:
                continue
            self.events.append((time.monotonic(), ev))

    def wait_new(self, pid, since, timeout=30):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            for t, ev in self.events:
                c = ev.get("container", {})
                if t >= since and ev.get("change") == "new" and c.get("pid") == pid:
                    return t, c["id"]
            time.sleep(0.002)
        raise TimeoutError(f"no window for pid {pid}")


def run_one(watcher, notches, cmd, shot=None):
    t0 = time.monotonic()
    p = subprocess.Popen(cmd, shell=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                         start_new_session=True)
    t_map, con = watcher.wait_new(p.pid, t0)
    out = {"map_ms": (t_map - t0) * 1000}
    # The headless output is 1280x820 points and the window fills it.
    time.sleep(4)
    out["rss_mb"] = rss_mb(p.pid)
    c0 = cpu_ms(p.pid)
    time.sleep(10)
    out["idle_cpu_ms"] = cpu_ms(p.pid) - c0

    pointer = VPointer()
    # Over the grid, between the 220 pt sidebar and the 340 pt details panel.
    pointer.move_to(580, 400)
    time.sleep(0.5)
    c0 = cpu_ms(p.pid)
    for _ in range(notches):
        pointer.wheel(1)
        time.sleep(4.0 / notches)
    time.sleep(1.5)
    pointer.close()
    out["scroll_cpu_ms"] = cpu_ms(p.pid) - c0
    out["rss_after_mb"] = rss_mb(p.pid)
    if shot:
        subprocess.run(["grim", shot])
    os.killpg(p.pid, 15)
    p.wait(timeout=10)
    time.sleep(1)
    return out


def main():
    rounds = int(sys.argv[1])
    apps = [a.split("=", 2) for a in sys.argv[2:]]
    watcher = Watcher()
    results = {name: [] for name, _, _ in apps}
    for i in range(rounds):
        order = apps if i % 2 == 0 else apps[::-1]
        for name, notches, cmd in order:
            r = run_one(watcher, int(notches), cmd, f"end-{name}-{i}.png" if i < 2 else None)
            results[name].append(r)
            print(name, i, {k: round(v, 1) for k, v in r.items()}, flush=True)
    print("\nmedians:")
    for name, runs in results.items():
        print(name, {k: round(statistics.median(r[k] for r in runs), 1) for k in runs[0]})
    json.dump(results, open("bench-results.json", "w"), indent=1)


main()
