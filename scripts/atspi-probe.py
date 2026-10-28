#!/usr/bin/env python3
"""Inspect or act on Velnopt's AT-SPI tree on the current session's accessibility bus.

Used by smoke-hyprland.py under a private D-Bus session; requires python-gobject and
at-spi2-core. `dump` prints JSON for applications whose toolkit is Velnopt.
`action WINDOW NAME ACTION` runs a named action (such as "click") on the first
node named NAME inside the window named WINDOW; "focus" grabs focus instead.
"""

import json
import sys
import warnings

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

# get_action_name is deprecated, and the Python bindings have no working replacement
warnings.filterwarnings("ignore", category=DeprecationWarning)


def velnopt_apps():
    desktop = Atspi.get_desktop(0)
    for index in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(index)
        if app is not None and app.get_toolkit_name() == "Velnopt":
            yield app


def describe(node, depth=0):
    states = node.get_state_set()
    entry = {
        "role": node.get_role_name(),
        "name": node.get_name(),
        "selected": states.contains(Atspi.StateType.SELECTED),
        "focused": states.contains(Atspi.StateType.FOCUSED),
        "actions": [],
        "children": [],
    }
    action = node.get_action_iface()
    if action is not None:
        entry["actions"] = [action.get_action_name(i) for i in range(action.get_n_actions())]
    if depth < 8:
        for index in range(min(node.get_child_count(), 512)):
            child = node.get_child_at_index(index)
            if child is not None:
                entry["children"].append(describe(child, depth + 1))
    return entry


def find(node, name, depth=0):
    if node.get_name() == name:
        return node
    if depth >= 8:
        return None
    for index in range(min(node.get_child_count(), 512)):
        child = node.get_child_at_index(index)
        found = child is not None and find(child, name, depth + 1)
        if found:
            return found
    return None


def main():
    Atspi.init()
    if sys.argv[1:] == ["dump"]:
        print(json.dumps([describe(app) for app in velnopt_apps()]))
        return 0
    if len(sys.argv) == 5 and sys.argv[1] == "action":
        _, _, window, name, wanted = sys.argv
        for app in velnopt_apps():
            scope = find(app, window)
            node = scope and find(scope, name)
            # focus goes through the Component interface, not an action
            if node and wanted == "focus":
                component = node.get_component_iface()
                return 0 if component and component.grab_focus() else 1
            action = node and node.get_action_iface()
            if action:
                for index in range(action.get_n_actions()):
                    if action.get_action_name(index) == wanted:
                        return 0 if action.do_action(index) else 1
        print(f"no {wanted!r} action on {name!r}", file=sys.stderr)
        return 1
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
