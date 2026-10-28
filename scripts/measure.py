#!/usr/bin/env python3
"""Measure startup, idle CPU, wakeups, and memory in a disposable nested Hyprland.

Runs each workload `--runs` times on a headless 1920x1080 output at scale 1:
Velnopt with examples/hyprland.toml (static text and a clock), Velnopt with
examples/desktop.toml (services on a private bus with no daemons), and, when
Quickshell is installed, scripts/quickshell-static/shell.qml, which draws the same
static panel. Startup is process launch until Hyprland lists the panel's layer
(polled every 5 ms; includes hyprctl latency, not frame presentation). Idle
figures are taken over `--idle` seconds after a 3-second settle. Writes JSON to
target/measurements/ and prints a summary. Requires Hyprland, hyprctl, dbus-daemon.
"""

import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
TICKS = os.sysconf("SC_CLK_TCK")


def wait_for(predicate, timeout=15, interval=0.05):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(interval)
    raise RuntimeError("timed out")


def stop(process):
    if process and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def cpu_ticks(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return int(fields[11]) + int(fields[12])  # utime + stime


def switches(pid):
    total = 0
    for task in Path(f"/proc/{pid}/task").iterdir():
        for line in (task / "status").read_text().splitlines():
            if line.startswith(("voluntary_ctxt_switches", "nonvoluntary_ctxt_switches")):
                total += int(line.split()[1])
    return total


def memory(pid):
    values = {}
    for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines():
        key, _, rest = line.partition(":")
        if key in ("Rss", "Pss"):
            values[key.lower() + "_kib"] = int(rest.split()[0])
    return values


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/velnopt")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--idle", type=int, default=30)
    args = parser.parse_args()
    if not args.binary.is_file():
        parser.error("build first: cargo build --release --locked -p velnopt-cli")
    workloads = [
        ("velnopt-static", [str(args.binary), "run", str(ROOT / "examples/hyprland.toml")]),
        ("velnopt-desktop", [str(args.binary), "run", str(ROOT / "examples/desktop.toml")]),
    ]
    if shutil.which("qs"):
        workloads.append(("quickshell-static", ["qs", "-p", str(ROOT / "scripts/quickshell-static/shell.qml")]))
    results = {}
    with tempfile.TemporaryDirectory(prefix="velnopt-measure-") as temporary:
        folder = Path(temporary)
        env = dict(os.environ, AQ_DRM_DEVICES="/dev/null", HYPRLAND_NO_SD_VARS="1", HYPRLAND_NO_SD_NOTIFY="1")
        env.pop("HYPRLAND_INSTANCE_SIGNATURE", None)
        for key in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
            (folder / key).mkdir()
            env[key] = str(folder / key)
        config = folder / "hyprland.conf"
        config.write_text("monitor = ,1280x720@60,auto,1\nmonitor = MEASURE-1,1920x1080@60,auto,1\n"
                          "animations {\n enabled = false\n}\nmisc {\n disable_hyprland_logo = true\n disable_splash_rendering = true\n}\n"
                          "xwayland {\n enabled = false\n}\n")
        # no service directories, so the workloads can't activate anything on the host
        bus_config = folder / "bus.conf"
        bus_config.write_text(f'<busconfig><type>session</type><listen>unix:dir={folder}</listen><auth>EXTERNAL</auth>'
                              '<policy context="default"><allow send_destination="*" eavesdrop="true"/>'
                              '<allow eavesdrop="true"/><allow own="*"/></policy></busconfig>')
        bus = subprocess.Popen(["dbus-daemon", f"--config-file={bus_config}", "--nofork", "--print-address=1"], stdout=subprocess.PIPE, text=True, env=env)
        address = bus.stdout.readline().strip()
        compositor = subprocess.Popen(["Hyprland", "--config", str(config)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            instance = wait_for(lambda: next((item for item in json.loads(subprocess.check_output(["hyprctl", "instances", "-j"], text=True)) if item["pid"] == compositor.pid), None))
            env.update(WAYLAND_DISPLAY=instance["wl_socket"], HYPRLAND_INSTANCE_SIGNATURE=instance["instance"])

            def ctl(*arguments):
                return subprocess.check_output(["hyprctl", "-i", instance["instance"], "-j", *arguments], text=True, env=env)

            wait_for(lambda: json.loads(ctl("monitors")))
            subprocess.check_output(["hyprctl", "-i", instance["instance"], "output", "create", "headless", "MEASURE-1"], env=env)
            wait_for(lambda: any(item["name"] == "MEASURE-1" for item in json.loads(ctl("monitors"))))
            # services and the tray get a private bus with no daemons; audio has no server
            run_env = dict(env, DBUS_SESSION_BUS_ADDRESS=address, DBUS_SYSTEM_BUS_ADDRESS=address,
                           PULSE_SERVER=f"unix:{folder}/none")

            def layer_of(pid):
                return any(layer.get("pid") == pid for monitor in json.loads(ctl("layers")).values()
                           for group in monitor["levels"].values() for layer in group)

            for name, command in workloads:
                samples = []
                for _ in range(args.runs):
                    started = time.monotonic()
                    process = subprocess.Popen(command, env=run_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    try:
                        wait_for(lambda: layer_of(process.pid), interval=0.005)
                        startup = time.monotonic() - started
                        time.sleep(3)
                        ticks, wakeups = cpu_ticks(process.pid), switches(process.pid)
                        time.sleep(args.idle)
                        sample = {
                            "startup_ms": round(startup * 1000, 1),
                            "idle_cpu_ms": round((cpu_ticks(process.pid) - ticks) * 1000 / TICKS, 1),
                            "idle_context_switches": switches(process.pid) - wakeups,
                            "threads": len(os.listdir(f"/proc/{process.pid}/task")),
                            **memory(process.pid),
                        }
                    finally:
                        stop(process)
                    samples.append(sample)
                summary = {key: {"median": statistics.median(sample[key] for sample in samples),
                                 "min": min(sample[key] for sample in samples),
                                 "max": max(sample[key] for sample in samples)}
                           for key in samples[0]}
                # repo-relative paths keep the checkout location out of the reports
                recorded = [argument.removeprefix(f"{ROOT}/") for argument in command]
                results[name] = {"command": recorded, "samples": samples, "summary": summary}
                print(name, json.dumps({key: value["median"] for key, value in summary.items()}))
        finally:
            stop(compositor)
            stop(bus)
    report = {
        "captured_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "host": {"cpu": next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")), ""),
                 "kernel": platform.release()},
        "hyprland": subprocess.check_output(["Hyprland", "--version"], text=True).splitlines()[0],
        "quickshell": subprocess.check_output(["qs", "--version"], text=True).strip() if shutil.which("qs") else None,
        "runs": args.runs, "idle_seconds": args.idle, "output": "headless 1920x1080, scale 1",
        "results": results,
    }
    destination = ROOT / "target/measurements"
    destination.mkdir(parents=True, exist_ok=True)
    path = destination / f"measurements-{int(time.time())}.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Wrote {path}")


if __name__ == "__main__":
    main()
