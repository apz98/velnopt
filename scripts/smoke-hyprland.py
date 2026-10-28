#!/usr/bin/env python3
"""Exercise a real panel in a disposable, nested Hyprland instance.

Requires Hyprland, hyprctl, grim, system fonts, and a running Wayland session or
labwc (used headless when no session is running).
Only the nested instance is configured; screenshots contain its test outputs.
"""

import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time


ROOT = Path(__file__).resolve().parent.parent


def wait_for(description, predicate, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.1)
    raise RuntimeError(f"timed out waiting for {description}")


def worker_threads(pid, name):
    """Count threads whose kernel name starts with `name` (truncated to 15 bytes)."""
    count = 0
    for task in Path(f"/proc/{pid}/task").iterdir():
        try:
            count += (task / "comm").read_text().strip() == name[:15]
        except OSError:
            pass
    return count


def private_accessibility_bus(folder, env):
    """Start a disposable session bus with an enabled AT-SPI bus; return processes."""
    # no service directories, so nothing on the host session can be activated
    bus_config = folder / "session-bus.conf"
    bus_config.write_text(f"""<busconfig>
  <type>session</type>
  <listen>unix:dir={folder}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
""")
    bus = subprocess.Popen(["dbus-daemon", f"--config-file={bus_config}", "--nofork", "--print-address=1"], stdout=subprocess.PIPE, text=True, env=env)
    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
    # keep the disposable bus from activating GVFS or dconf on the host session
    env.update(GIO_USE_VFS="local", GSETTINGS_BACKEND="memory")
    launcher = subprocess.Popen(["/usr/lib/at-spi-bus-launcher", "--launch-immediately"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def enabled():
        result = subprocess.run(["busctl", "--address", env["DBUS_SESSION_BUS_ADDRESS"], "set-property", "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Status", "IsEnabled", "b", "true"], env=env, capture_output=True, timeout=5)
        return result.returncode == 0

    wait_for("private accessibility bus", enabled)
    return [launcher, bus]


def stop(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/velnopt")
    parser.add_argument("--artifacts", type=Path, default=ROOT / "target/hyprland-smoke")
    parser.add_argument("--clock-tick", action="store_true", help="also wait up to one minute to verify a visible clock update")
    parser.add_argument("--live-widgets", action="store_true", help="also verify real workspace and focused-window updates (requires foot)")
    parser.add_argument("--interactive", action="store_true", help="also verify pointer, keyboard, and AT-SPI workspace controls (requires wtype, dbus-daemon, at-spi2-core, and python-gobject)")
    parser.add_argument("--pointer-helper", type=Path, default=ROOT / "target/release/examples/virtual-pointer", help="built with: cargo build --release --locked -p velnopt-wayland --example virtual-pointer")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error("build the executable first: cargo build --release --locked")
    for tool in ("Hyprland", "hyprctl", "grim"):
        if not shutil.which(tool):
            parser.error(f"{tool} is required")
    if args.interactive:
        for tool in ("wtype", "dbus-daemon", "busctl"):
            if not shutil.which(tool):
                parser.error(f"{tool} is required for --interactive")
        if not args.pointer_helper.is_file():
            parser.error("build the pointer helper: cargo build --release --locked -p velnopt-wayland --example virtual-pointer")
    if args.live_widgets and not shutil.which("foot"):
        parser.error("foot is required for the live-widget window test")
    headless = not os.environ.get("WAYLAND_DISPLAY")
    if headless and not shutil.which("labwc"):
        parser.error("run this test inside a Wayland session, or install labwc to run it headless")
    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    for filename in ("report.json", "failure-monitors.json", "failure-layers.json"):
        (artifacts / filename).unlink(missing_ok=True)
    compositor = panel = terminal = parent = held_pointer = held_keyboard = None
    helpers = []
    results = []
    with tempfile.TemporaryDirectory(prefix="velnopt-hyprland-") as temporary:
        folder = Path(temporary)
        env = dict(os.environ, AQ_DRM_DEVICES="/dev/null", HYPRLAND_NO_SD_VARS="1", HYPRLAND_NO_SD_NOTIFY="1")
        env.pop("HYPRLAND_INSTANCE_SIGNATURE", None)
        for key in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"):
            location = folder / key.lower()
            location.mkdir()
            env[key] = str(location)
        compositor_config = folder / "hyprland.conf"
        compositor_config.write_text("""monitor = ,1280x720@60,auto,1
animations {
    enabled = false
}
misc {
    disable_hyprland_logo = true
    disable_splash_rendering = true
}
xwayland {
    enabled = false
}
""" + ("".join(f"workspace = {number}, monitor:VELNOPT-TEST{', persistent:true' if number < 4 else ''}\n" for number in range(1, 6)) if args.interactive else ""))
        config = folder / "panel.toml"
        original = (ROOT / "examples/hyprland.toml").read_text()
        config.write_text(original)
        try:
            if headless:
                # outside a session, nest in a headless labwc: cage, sway, and weston
                # lack the xdg_wm_base 6 that Hyprland's Wayland backend binds
                socket_file = folder / "parent-socket"
                with (artifacts / "parent.log").open("w") as log:
                    parent = subprocess.Popen(["labwc", "-s", f"sh -c 'echo \"$WAYLAND_DISPLAY\" > \"{socket_file}\"'"],
                                              env=dict(env, WLR_BACKENDS="headless", WLR_LIBINPUT_NO_DEVICES="1"),
                                              stdout=log, stderr=subprocess.STDOUT)
                env["WAYLAND_DISPLAY"] = wait_for("headless labwc", lambda: socket_file.is_file() and socket_file.read_text().strip())
            with (artifacts / "compositor.log").open("w") as log:
                compositor = subprocess.Popen(["Hyprland", "--config", str(compositor_config)], env=env, stdout=log, stderr=subprocess.STDOUT)

            def find_instance():
                if compositor.poll() is not None:
                    raise RuntimeError("nested Hyprland exited; inspect compositor.log")
                instances = json.loads(subprocess.check_output(["hyprctl", "instances", "-j"], text=True, timeout=5))
                return next((entry for entry in instances if entry["pid"] == compositor.pid), None)

            instance = wait_for("nested Hyprland", find_instance)
            env.update(WAYLAND_DISPLAY=instance["wl_socket"], HYPRLAND_INSTANCE_SIGNATURE=instance["instance"])

            def ctl(*arguments, json_output=False):
                command = ["hyprctl", "-i", instance["instance"]]
                if json_output:
                    command.append("-j")
                output = subprocess.check_output(command + list(arguments), text=True, timeout=5, env=env)
                return json.loads(output) if json_output else output.strip()

            # a headless parent never maps the backend's window output
            nested = [] if headless else wait_for("an initialized nested monitor", lambda: ctl("monitors", json_output=True))
            # the nested backend's output is a window on the host desktop, where the host
            # pointer can move focus mid-test, so swap it for a second headless output
            assert ctl("output", "create", "headless", "VELNOPT-OTHER") == "ok"
            for item in nested:
                assert ctl("output", "remove", item["name"]) == "ok"
            wait_for("headless outputs only", lambda: [item["name"] for item in ctl("monitors", json_output=True)] == ["VELNOPT-OTHER"])
            if headless and args.interactive:
                held_pointer = subprocess.Popen([str(args.pointer_helper), "hold"], env=env)
                wait_for("a pointer device", lambda: ctl("devices", json_output=True)["mice"])
                # same for a keyboard: wtype sleeps for a day before its only key, and the test
                # stops it long before then
                held_keyboard = subprocess.Popen(["wtype", "-s", "86400000", "-k", "F24"], env=env)
                wait_for("a keyboard device", lambda: any(item["name"] != "virtual-pointer" for item in ctl("devices", json_output=True)["keyboards"]))
            assert ctl("output", "create", "headless", "VELNOPT-TEST") == "ok"
            wait_for("headless test monitor", lambda: any(item["name"] == "VELNOPT-TEST" for item in ctl("monitors", json_output=True)))

            def layers():
                if panel is not None and panel.poll() is not None:
                    raise RuntimeError("panel exited unexpectedly; inspect panel.log")
                output = ctl("layers", json_output=True)
                return {name: [layer for group in data["levels"].values() for layer in group if layer["namespace"] == "velnopt"] for name, data in output.items()}

            def layer_on(name, height=None):
                candidates = layers().get(name, [])
                return next((layer for layer in candidates if height is None or layer["h"] == height), None)

            def capture(name, output="VELNOPT-TEST", crop=False, group=None):
                destination = artifacts / f"{name}.png"
                command = ["grim"]
                if crop or group is not None:
                    layer = layer_on(output)
                    assert layer is not None
                    x, width = layer["x"], layer["w"]
                    if group is not None:
                        x += width * group // 3
                        width = layer["w"] * (group + 1) // 3 - layer["w"] * group // 3
                    command += ["-g", f"{x},{layer['y']} {width}x{layer['h']}"]
                else:
                    command += ["-o", output]
                subprocess.run(command + [str(destination)], check=True, env=env, timeout=10)
                data = destination.read_bytes()
                assert data[:8] == b"\x89PNG\r\n\x1a\n"
                return struct.unpack(">II", data[16:24])

            def log_text():
                return (artifacts / "panel.log").read_text()

            base_compositor_config = compositor_config.read_text()

            def set_test_mode(width, height, scale):
                # keep monitor changes in the nested configuration, so a compositor reload
                # can't bring back the original wildcard rule mid-test
                compositor_config.write_text(base_compositor_config + f"\nmonitor = VELNOPT-TEST,{width}x{height}@60,auto,{scale}\n")
                assert ctl("reload") == "ok"
                wait_for("compositor test output mode", lambda: any(
                    item["name"] == "VELNOPT-TEST" and item["width"] == width
                    and item["height"] == height and item["scale"] == scale
                    for item in ctl("monitors", json_output=True)
                ))

            def reload_with(source, expected):
                config.write_text(source)
                previous = len(log_text())
                panel.send_signal(signal.SIGHUP)
                wait_for(expected, lambda: expected in log_text()[previous:])

            if args.interactive:
                helpers = private_accessibility_bus(folder, env)
                # services see no host daemons: the system bus is the private bus, Pulse points
                # at a missing socket, and applications come from a test-only data directory
                env["DBUS_SYSTEM_BUS_ADDRESS"] = env["DBUS_SESSION_BUS_ADDRESS"]
                env["PULSE_SERVER"] = f"unix:{folder}/no-pulse-server"
                applications = folder / "data/applications"
                applications.mkdir(parents=True)
                (applications / "velnopt-smoke.desktop").write_text(
                    "[Desktop Entry]\nType=Application\nName=Velnopt Smoke Launch\n"
                    # the child records its blocked signals; Velnopt's signalfd mask must not leak into it
                    f"Exec=sh -c \"grep SigBlk /proc/self/status > '{folder}/launched; not a shell'\"\n")
                env["XDG_DATA_DIRS"] = str(folder / "data")
            with (artifacts / "panel.log").open("w") as log:
                panel = subprocess.Popen([str(binary), "run", str(config)], env=env, stdout=subprocess.DEVNULL, stderr=log)
            wait_for("initial panel", lambda: layer_on("VELNOPT-TEST", 36))
            wait_for("initial presentation", lambda: any(item["reserved"][1] == 36 for item in ctl("monitors", json_output=True) if item["name"] == "VELNOPT-TEST"))
            time.sleep(0.2)
            capture("desktop")
            capture("panel", crop=True)
            assert all(len(items) == 1 for items in layers().values())
            results.append("initial rendering and one panel per output")

            if args.clock_tick:
                minute = int(time.time() // 60)
                wait_for("the next clock minute", lambda: int(time.time() // 60) != minute, timeout=65)
                time.sleep(0.3)
                capture("clock-tick", crop=True)
                assert (artifacts / "panel.png").read_bytes() != (artifacts / "clock-tick.png").read_bytes()
                results.append("clock visibly updates at a minute boundary")

            if args.live_widgets:
                live = (ROOT / "examples/live-hyprland.toml").read_text()
                reload_with(live, "reloaded ")
                wait_for("live IPC connection", lambda: "Hyprland IPC connected" in log_text())
                wait_for("one IPC worker", lambda: worker_threads(panel.pid, "velnopt-hyprland") == 1)
                assert ctl("dispatch", "focusmonitor", "VELNOPT-TEST") == "ok"
                assert ctl("dispatch", "workspace", "4") == "ok"
                wait_for("workspace 4", lambda: ctl("activeworkspace", json_output=True).get("id") == 4)
                time.sleep(0.3)
                capture("workspace-before", group=0)
                assert ctl("dispatch", "workspace", "5") == "ok"
                wait_for("workspace 5", lambda: ctl("activeworkspace", json_output=True).get("id") == 5)

                def workspace_changed():
                    capture("workspace-after", group=0)
                    return (artifacts / "workspace-before.png").read_bytes() != (artifacts / "workspace-after.png").read_bytes()

                wait_for("visible workspace update", workspace_changed)
                capture("live-workspaces", crop=True)
                results.append("live workspace change updates the panel without reload")

                fifo = folder / "window-title"
                os.mkfifo(fifo)
                initial_title = "Velnopt IPC test initial"
                updated_title = "Velnopt IPC test updated, punctuation >> and a long title for clipping"
                shell = 'read -r title < "$1"; printf "\\033]0;%s\\007" "$title"; exec sleep 60'
                with (artifacts / "terminal.log").open("w") as log:
                    terminal = subprocess.Popen(["foot", "--log-no-syslog", "--title", initial_title, "--app-id", "velnopt-smoke", "sh", "-c", shell, "velnopt-title-test", str(fifo)], env=env, stdout=log, stderr=subprocess.STDOUT)
                wait_for("focused test window", lambda: ctl("activewindow", json_output=True).get("title") == initial_title)
                time.sleep(0.3)
                capture("title-before", group=1)

                def send_title():
                    try:
                        descriptor = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
                    except OSError:
                        return False
                    with os.fdopen(descriptor, "w") as stream:
                        stream.write(updated_title + "\n")
                    return True

                wait_for("terminal title command", send_title)
                wait_for("updated native window title", lambda: ctl("activewindow", json_output=True).get("title") == updated_title)

                def title_changed():
                    capture("title-after", group=1)
                    return (artifacts / "title-before.png").read_bytes() != (artifacts / "title-after.png").read_bytes()

                wait_for("visible window title update", title_changed)
                capture("live-title", crop=True)
                results.append("focused window title changes render without reload")
                set_test_mode(480, 320, 1)
                wait_for("narrow live panel", lambda: (layer_on("VELNOPT-TEST") or {}).get("w") == 480)
                time.sleep(0.3)
                capture("live-narrow", crop=True)
                set_test_mode(1280, 720, 1)
                wait_for("restored live panel", lambda: (layer_on("VELNOPT-TEST") or {}).get("w") == 1280)
                stop(terminal)
                terminal = None
                wait_for("focused window closed", lambda: not ctl("activewindow", json_output=True))
                time.sleep(0.3)
                capture("live-no-window", crop=True)
                results.append("live title clipping and no-focused-window state")
                reload_with(original, "reloaded ")
                wait_for("IPC worker stopped after disabling live widgets", lambda: worker_threads(panel.pid, "velnopt-hyprland") == 0)
                results.append("reload disables and joins the optional IPC worker")

            if args.interactive:
                interactive_checks(ctl, layer_on, capture, reload_with, log_text, set_test_mode, panel, env, args.pointer_helper, artifacts, results)
                desktop_checks(ctl, layer_on, capture, reload_with, set_test_mode, panel, env, args.pointer_helper, artifacts, folder, helpers, results)
                reload_with(original, "reloaded ")
                workers = ("velnopt-hyprland", "velnopt-dbus", "velnopt-audio", "velnopt-tray")
                wait_for("idle workers joined", lambda: all(worker_threads(panel.pid, name) == 0 for name in workers))
                def thread_names():
                    names = []
                    for task in Path(f"/proc/{panel.pid}/task").iterdir():
                        try:
                            names.append((task / "comm").read_text().strip())
                        except OSError:
                            pass
                    return sorted(names)

                descriptors = len(os.listdir(f"/proc/{panel.pid}/fd"))
                threads = thread_names()
                desktop = (ROOT / "examples/desktop.toml").read_text()
                for _ in range(10):
                    reload_with(desktop, "reloaded ")
                    wait_for("service workers", lambda: worker_threads(panel.pid, "velnopt-dbus") == 1 and worker_threads(panel.pid, "velnopt-tray") == 1)
                    reload_with(original, "reloaded ")
                    wait_for("workers joined after reload", lambda: all(worker_threads(panel.pid, name) == 0 for name in workers))
                # zbus's `blocking` pool keeps idle threads up to its peak concurrency (it only
                # shrinks above 500 threads), so its size doesn't mean a reload leaked
                def own_threads():
                    return [name for name in thread_names() if not re.fullmatch(r"blocking-\d+", name)]
                baseline = [name for name in threads if not re.fullmatch(r"blocking-\d+", name)]
                try:
                    wait_for("thread count back to baseline", lambda: len(own_threads()) <= len(baseline), timeout=20)
                except RuntimeError:
                    raise AssertionError(f"threads leaked across reloads: before {threads}, after {thread_names()}") from None
                # leave a little slack for compositor objects that settle asynchronously
                assert len(os.listdir(f"/proc/{panel.pid}/fd")) <= descriptors + 2, "descriptors leaked across reloads"
                assert layer_on("VELNOPT-TEST", 36)
                results.append("20 reloads start and join every IPC and service worker without leaking threads or descriptors")
                wait_for("IPC worker stopped after interactive checks", lambda: worker_threads(panel.pid, "velnopt-hyprland") == 0)

            reload_with("version = [", "reload rejected")
            assert layer_on("VELNOPT-TEST", 36)
            capture("invalid-reload", crop=True)
            results.append("invalid reload preserves the existing panel")

            changed = original.replace('edge = "top"', 'edge = "bottom"').replace("height = 36", "height = 44").replace('text = "Velnopt"', 'text = "Reloaded"')
            reload_with(changed, "reloaded ")
            layer = wait_for("bottom panel after reload", lambda: layer_on("VELNOPT-TEST", 44))
            assert layer["y"] > 0
            time.sleep(0.2)
            capture("reloaded", crop=True)
            results.append("valid reload updates text, height, and edge")

            reload_with(changed.replace("height = 44", "height = 65535"), "frame limit")
            assert layer_on("VELNOPT-TEST", 44)
            results.append("resource limit rejects an oversized reload")

            config.unlink()
            os.mkfifo(config)
            previous = len(log_text())
            panel.send_signal(signal.SIGHUP)
            wait_for("nonblocking FIFO rejection", lambda: "regular file" in log_text()[previous:])
            config.unlink()
            config.write_text(changed)
            results.append("FIFO reload rejects promptly")

            # pin it to a predictable headless output, then exercise output geometry
            selected = changed.replace('[panel]\n', '[panel]\noutput = "VELNOPT-TEST"\n')
            reload_with(selected, "reloaded ")
            wait_for("output selection", lambda: sum(len(items) for items in layers().values()) == 1)
            assert layer_on("VELNOPT-TEST", 44)
            set_test_mode(480, 320, 1)
            wait_for("narrow output resize", lambda: (layer_on("VELNOPT-TEST") or {}).get("w") == 480)
            time.sleep(0.2)
            assert capture("narrow", crop=True) == (480, 44)
            results.append("output selection and narrow resize")

            set_test_mode(1280, 720, 2)
            wait_for("HiDPI resize", lambda: (layer_on("VELNOPT-TEST") or {}).get("w") == 640)
            time.sleep(0.2)
            assert capture("hidpi", crop=True) == (1280, 88)
            results.append("integer HiDPI scale")

            set_test_mode(1280, 720, 1.25)
            wait_for("fractional resize", lambda: (layer_on("VELNOPT-TEST") or {}).get("w") == 1024)
            time.sleep(0.2)
            capture("fractional", crop=True)
            results.append("fractional scaling through compositor resampling")

            reload_with(original, "reloaded ")
            wait_for("all outputs selected", lambda: sum(len(items) for items in layers().values()) == 2)
            assert ctl("output", "remove", "VELNOPT-TEST") == "ok"
            wait_for("output removal", lambda: sum(len(items) for items in layers().values()) == 1)
            assert ctl("output", "create", "headless", "VELNOPT-TEST") == "ok"
            wait_for("output reconnection", lambda: sum(len(items) for items in layers().values()) == 2)
            results.append("output removal and reconnection")

            panel.terminate()
            assert panel.wait(timeout=10) == 0
            panel = None
            wait_for("layer cleanup", lambda: not any(layers().values()))
            results.append("SIGTERM exits successfully and removes every layer")

            if args.live_widgets:
                config.write_text((ROOT / "examples/live-hyprland.toml").read_text())
                offline_env = dict(env)
                offline_env.pop("HYPRLAND_INSTANCE_SIGNATURE", None)
                with (artifacts / "offline.log").open("w") as log:
                    panel = subprocess.Popen([str(binary), "run", str(config)], env=offline_env, stdout=subprocess.DEVNULL, stderr=log)
                wait_for("offline live-widget panel", lambda: layer_on("VELNOPT-TEST", 36))
                wait_for("explicit IPC unavailable state", lambda: "Hyprland IPC unavailable" in (artifacts / "offline.log").read_text())
                time.sleep(0.2)
                capture("live-offline", crop=True)
                panel.terminate()
                assert panel.wait(timeout=10) == 0
                panel = None
                results.append("unavailable IPC keeps the panel usable and shutdown responsive")
                config.write_text(original)

            with (artifacts / "disconnect.log").open("w") as log:
                panel = subprocess.Popen([str(binary), "run", str(config)], env=env, stdout=subprocess.DEVNULL, stderr=log)
            wait_for("panel before disconnect", lambda: layer_on("VELNOPT-TEST", 36))
            stop(compositor)
            assert panel.wait(timeout=10) == 1
            assert "panicked" not in (artifacts / "disconnect.log").read_text()
            results.append("compositor disconnect reports failure without a panic")
            report = {
                "hyprland": json.loads(subprocess.check_output(["Hyprland", "--version-json"], text=True)),
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "captured_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "checks": results,
            }
            (artifacts / "report.json").write_text(json.dumps(report, indent=2) + "\n")
            print(f"Passed {len(results)} Hyprland integration checks. Artifacts: {artifacts}")
            for result in results:
                print(f"  {result}")
        except Exception:
            if "ctl" in locals() and compositor is not None and compositor.poll() is None:
                for query in ("monitors", "layers"):
                    try:
                        (artifacts / f"failure-{query}.json").write_text(json.dumps(ctl(query, json_output=True), indent=2) + "\n")
                    except (OSError, ValueError, subprocess.SubprocessError):
                        pass
            raise
        finally:
            stop(terminal)
            stop(panel)
            stop(held_keyboard)
            stop(held_pointer)
            stop(compositor)
            stop(parent)
            for helper in helpers:
                stop(helper)


def interactive_checks(ctl, layer_on, capture, reload_with, log_text, set_test_mode, panel, env, pointer_helper, artifacts, results):
    """Drive workspace controls with a virtual pointer, wtype, and AT-SPI."""
    probe = [sys.executable, str(ROOT / "scripts/atspi-probe.py")]
    window = "Velnopt panel on VELNOPT-TEST"
    offset = len(log_text())
    reload_with((ROOT / "examples/live-hyprland.toml").read_text(), "reloaded ")
    wait_for("live IPC connection", lambda: "Hyprland IPC connected" in log_text()[offset:])
    assert ctl("dispatch", "focusmonitor", "VELNOPT-TEST") == "ok"

    def active():
        return ctl("activeworkspace", json_output=True).get("id")

    def show(number):
        assert ctl("dispatch", "workspace", str(number)) == "ok"
        wait_for(f"workspace {number}", lambda: active() == number)

    def keys(*names):
        for name in names:
            subprocess.run(["wtype", "-k", name], check=True, env=env, timeout=5)

    def differs(name, reference, group=0):
        def changed():
            capture(name, group=group)
            return (artifacts / f"{name}.png").read_bytes() != (artifacts / f"{reference}.png").read_bytes()
        return changed

    def click_first_workspace():
        layer = layer_on("VELNOPT-TEST")
        # the first workspace label starts at the example's 18 px left padding
        assert ctl("dispatch", "movecursor", str(layer["x"] + 21), str(layer["y"] + layer["h"] // 2)) == "ok"
        subprocess.run([str(pointer_helper), "click"], check=True, env=env, timeout=5)

    show(3)
    click_first_workspace()
    wait_for("workspace 1 after a click", lambda: active() == 1)
    set_test_mode(1280, 720, 2)
    wait_for("HiDPI panel", lambda: (layer_on("VELNOPT-TEST") or {}).get("w") == 640)
    show(3)
    time.sleep(0.3)
    click_first_workspace()
    wait_for("workspace 1 after a HiDPI click", lambda: active() == 1)
    set_test_mode(1280, 720, 1)
    wait_for("restored panel", lambda: (layer_on("VELNOPT-TEST") or {}).get("w") == 1280)
    results.append("clicking a workspace switches it through validated Hyprland IPC at scales 1 and 2")

    time.sleep(0.3)
    capture("keyboard-idle", group=0)
    keys("Right", "Return")
    time.sleep(0.5)
    assert active() == 1, "keys must not reach the panel before keyboard mode"
    panel.send_signal(signal.SIGUSR1)
    wait_for("visible focus ring", differs("keyboard-focus", "keyboard-idle"))
    keys("Right", "Return")
    wait_for("workspace 2 from the keyboard", lambda: active() == 2)
    keys("Left", "Left", "Return")
    wait_for("wrapped keyboard focus to workspace 3", lambda: active() == 3)
    capture("keyboard-navigation", crop=True)
    keys("Escape")
    time.sleep(0.3)
    capture("keyboard-released", crop=True)
    keys("Left", "Return")
    time.sleep(0.5)
    assert active() == 3, "Escape must release keyboard focus"
    results.append("keyboard mode shows focus, navigates, activates, wraps, and releases on Escape")

    def tabs():
        apps = json.loads(subprocess.check_output(probe + ["dump"], env=env, text=True, timeout=10))
        found = []

        def walk(node, parent, inside):
            inside = inside or node["name"] == window
            if inside and node["role"] == "page tab":
                found.append((parent, node))
            for child in node["children"]:
                walk(child, node["role"], inside)
        for app in apps:
            walk(app, None, False)
        return found

    found = wait_for("AT-SPI workspace tabs", lambda: len(tabs()) >= 3 and tabs())
    (artifacts / "atspi.json").write_text(json.dumps(json.loads(subprocess.check_output(probe + ["dump"], env=env, text=True, timeout=10)), indent=2) + "\n")
    names = [node["name"] for _, node in found]
    assert names[:3] == ["Workspace 1", "Workspace 2", "Workspace 3"], names
    assert all(parent == "page tab list" for parent, _ in found)
    assert [node["selected"] for _, node in found][:3] == [False, False, True]
    assert all("click" in node["actions"] for _, node in found), found
    assert subprocess.run(probe + ["action", window, "Workspace 1", "click"], env=env, timeout=10).returncode == 0
    wait_for("workspace 1 from AT-SPI", lambda: active() == 1)
    wait_for("AT-SPI selection update", lambda: [node["selected"] for _, node in tabs()][:3] == [True, False, False])
    assert ctl("dispatch", "renameworkspace", "2", "mail") == "ok"
    wait_for("renamed workspace in AT-SPI", lambda: [node["name"] for _, node in tabs()][:3] == ["Workspace 1", "Workspace mail", "Workspace 3"])
    results.append("AT-SPI exposes a workspace tab list with selection, renames, and click actions")


def desktop_checks(ctl, layer_on, capture, reload_with, set_test_mode, panel, env, pointer_helper, artifacts, folder, helpers, results):
    """Service states, tray item and menu, and launcher in an isolated session."""
    probe = [sys.executable, str(ROOT / "scripts/atspi-probe.py")]
    window = "Velnopt panel on VELNOPT-TEST"
    reload_with((ROOT / "examples/desktop.toml").read_text(), "reloaded ")

    def nodes():
        apps = json.loads(subprocess.check_output(probe + ["dump"], env=env, text=True, timeout=10))
        found = []

        def walk(node, inside):
            inside = inside or node["name"] == window
            if inside:
                found.append(node)
            for child in node["children"]:
                walk(child, inside)
        for app in apps:
            walk(app, False)
        return found

    # AT-SPI role names differ between at-spi2-core versions
    aliases = {"button": {"button", "push button"}, "menu item": {"menu item", "check menu item"},
               "search": {"entry", "search", "text"}, "list item": {"list item", "option"}}

    def has(role, name):
        return any(node["role"] in aliases.get(role, {role}) and node["name"] == name for node in nodes())

    def act(name, action):
        assert subprocess.run(probe + ["action", window, name, action], env=env, timeout=10).returncode == 0

    def keys(*names):
        for name in names:
            subprocess.run(["wtype", "-k", name], check=True, env=env, timeout=5)

    for text in ("Network unavailable", "Bluetooth unavailable", "Audio unavailable", "Battery unavailable"):
        wait_for(f"{text} state", lambda text=text: has("label", text))
    capture("services-unavailable", crop=True)
    results.append("service widgets show unavailable states without host daemons")

    lines = []
    mock = subprocess.Popen([sys.executable, str(ROOT / "scripts/mock-tray-item.py"), env["DBUS_SESSION_BUS_ADDRESS"]],
                            env=env, stdout=subprocess.PIPE, text=True)
    helpers.insert(0, mock)
    threading.Thread(target=lambda: lines.extend(mock.stdout), daemon=True).start()
    wait_for("tray item registration", lambda: "REGISTERED\n" in lines)
    try:
        wait_for("tray button", lambda: has("button", "Smoke item"))
    finally:
        (artifacts / "desktop-atspi.json").write_text(json.dumps([[node["role"], node["name"]] for node in nodes()], indent=1))
    time.sleep(0.3)
    capture("tray-icon", crop=True)
    act("Smoke item", "click")
    wait_for("tray activation", lambda: "ACTIVATE\n" in lines)
    act("Smoke item", "focus")
    time.sleep(0.3)
    keys("Return")
    wait_for("tray menu", lambda: has("menu item", "Open smoke"))
    assert not has("menu item", ""), "separators are not menu items"
    capture("tray-menu", crop=True)
    keys("Return")
    wait_for("menu event", lambda: "EVENT 1 clicked\n" in lines)
    wait_for("menu closed", lambda: not has("menu item", "Open smoke"))
    results.append("tray icon, activation, keyboard menu, and menu event through dbusmenu")

    panel.send_signal(signal.SIGUSR2)
    wait_for("launcher search field", lambda: has("search", "Search applications"))
    subprocess.run(["wtype", "smoke"], check=True, env=env, timeout=5)
    wait_for("launcher result", lambda: has("list item", "Velnopt Smoke Launch"))
    capture("launcher", crop=True)
    keys("Return")
    marker = folder / "launched; not a shell"
    wait_for("launched application", lambda: marker.exists() and marker.read_text().strip())
    assert marker.read_text().split() == ["SigBlk:", "0000000000000000"], marker.read_text()
    wait_for("launcher closed", lambda: not has("list item", "Velnopt Smoke Launch"))
    layer = layer_on("VELNOPT-TEST")
    assert ctl("dispatch", "movecursor", str(layer["x"] + 21), str(layer["y"] + layer["h"] // 2)) == "ok"
    subprocess.run([str(pointer_helper), "click"], check=True, env=env, timeout=5)
    wait_for("launcher from a click", lambda: has("list item", "Velnopt Smoke Launch"))
    keys("Escape")
    wait_for("launcher closed by Escape", lambda: not has("list item", "Velnopt Smoke Launch"))
    results.append("launcher opens by signal and click, searches, launches with an empty signal mask, and closes")
    set_test_mode(1280, 720, 1)
    wait_for("restored panel", lambda: (layer_on("VELNOPT-TEST") or {}).get("w") == 1280)


if __name__ == "__main__":
    main()
