#!/usr/bin/env python3
"""A StatusNotifierItem with a dbusmenu, for smoke-hyprland.py on a private session bus.

Registers with org.kde.StatusNotifierWatcher and prints one line per call it
receives (ACTIVATE, SECONDARY, EVENT <id> <name>) so the test can assert them.
Never run it on a real desktop session bus.
"""

import sys

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

NAME = "org.velnopt.SmokeTrayItem"
ITEM_XML = """<node>
  <interface name="org.kde.StatusNotifierItem">
    <method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
    <method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
    <method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="IconPixmap" type="a(iiay)" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
    <property name="Menu" type="o" access="read"/>
    <signal name="NewTitle"/>
  </interface>
</node>"""
MENU_XML = """<node>
  <interface name="com.canonical.dbusmenu">
    <method name="GetLayout">
      <arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="as" direction="in"/>
      <arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/>
    </method>
    <method name="AboutToShow"><arg type="i" direction="in"/><arg type="b" direction="out"/></method>
    <method name="Event">
      <arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/><arg type="u" direction="in"/>
    </method>
    <property name="Version" type="u" access="read"/>
    <property name="Status" type="s" access="read"/>
    <signal name="LayoutUpdated"><arg type="u"/><arg type="i"/></signal>
  </interface>
</node>"""


def entry(identifier, properties):
    values = {key: GLib.Variant("s", value) for key, value in properties.items()}
    return GLib.Variant("(ia{sv}av)", (identifier, values, []))


def main():
    address = sys.argv[1]
    flags = Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION
    bus = Gio.DBusConnection.new_for_address_sync(address, flags, None, None)
    red = bytes([255, 220, 40, 40]) * (16 * 16)  # ARGB, opaque red-ish
    properties = {
        "Id": GLib.Variant("s", "velnopt-smoke"),
        "Title": GLib.Variant("s", "Smoke item"),
        "Status": GLib.Variant("s", "Active"),
        "IconName": GLib.Variant("s", ""),
        "IconPixmap": GLib.Variant("a(iiay)", [(16, 16, red)]),
        "ItemIsMenu": GLib.Variant("b", False),
        "Menu": GLib.Variant("o", "/MenuBar"),
    }

    def item_call(_connection, _sender, _path, _interface, method, _parameters, invocation):
        print({"Activate": "ACTIVATE", "SecondaryActivate": "SECONDARY"}.get(method, method.upper()), flush=True)
        invocation.return_value(None)

    def item_property(_connection, _sender, _path, _interface, name):
        return properties[name]

    def menu_call(_connection, _sender, _path, _interface, method, parameters, invocation):
        if method == "GetLayout":
            root = (0, {"children-display": GLib.Variant("s", "submenu")}, [
                entry(1, {"label": "_Open smoke"}),
                entry(2, {"type": "separator"}),
                entry(3, {"label": "Quit smoke"}),
            ])
            invocation.return_value(GLib.Variant("(u(ia{sv}av))", (1, root)))
        elif method == "AboutToShow":
            invocation.return_value(GLib.Variant("(b)", (False,)))
        else:
            identifier, name = parameters.unpack()[:2]
            print(f"EVENT {identifier} {name}", flush=True)
            invocation.return_value(None)

    def menu_property(_connection, _sender, _path, _interface, name):
        return {"Version": GLib.Variant("u", 3), "Status": GLib.Variant("s", "normal")}[name]

    item_info = Gio.DBusNodeInfo.new_for_xml(ITEM_XML).interfaces[0]
    menu_info = Gio.DBusNodeInfo.new_for_xml(MENU_XML).interfaces[0]
    bus.register_object("/StatusNotifierItem", item_info, item_call, item_property, None)
    bus.register_object("/MenuBar", menu_info, menu_call, menu_property, None)
    bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                  GLib.Variant("(su)", (NAME, 4)), None, Gio.DBusCallFlags.NONE, 5000, None)

    def register():
        try:
            bus.call_sync("org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher", "org.kde.StatusNotifierWatcher",
                          "RegisterStatusNotifierItem", GLib.Variant("(s)", (NAME,)), None, Gio.DBusCallFlags.NONE, 2000, None)
            print("REGISTERED", flush=True)
            return False
        except GLib.Error:
            return True  # the panel's watcher isn't up yet; retry

    GLib.timeout_add(200, register)
    GLib.MainLoop().run()


if __name__ == "__main__":
    main()
