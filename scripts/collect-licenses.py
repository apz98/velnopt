#!/usr/bin/env python3
"""Collect host-resolved dependency metadata and supplied license notices."""

import json
from pathlib import Path
import shutil
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
# Notices for crates that ship without license files, copied from the upstream
# repository at the released tag. Only add an entry after reviewing it.
SUPPLEMENTAL = {
    "accesskit": "accesskit",
    "accesskit_atspi_common": "accesskit",
    "accesskit_consumer": "accesskit",
    "accesskit_unix": "accesskit",
}


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: collect-licenses.py DESTINATION")
    destination = Path(sys.argv[1])
    destination.mkdir(parents=True, exist_ok=True)
    version = subprocess.check_output(["rustc", "-vV"], text=True)
    host = next(line.removeprefix("host: ") for line in version.splitlines() if line.startswith("host: "))
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", host], cwd=ROOT))
    resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
    inventory = []
    for package in sorted(metadata["packages"], key=lambda item: (item["name"], item["version"])):
        if package["source"] is None or package["id"] not in resolved:
            continue
        folder = Path(package["manifest_path"]).parent
        notices = [path for path in folder.iterdir() if path.is_file() and path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT", "UNLICENSE"))]
        if package["license_file"]:
            notice = folder / package["license_file"]
            if notice.is_file() and notice not in notices:
                notices.append(notice)
        if not notices and package["name"] in SUPPLEMENTAL:
            notices = sorted((ROOT / "packaging/licenses" / SUPPLEMENTAL[package["name"]]).iterdir())
        if not notices:
            raise RuntimeError(f"missing license text for {package['name']} {package['version']}; review before packaging")
        entry = destination / f"{package['name']}-{package['version']}"
        entry.mkdir()
        for notice in notices:
            shutil.copyfile(notice, entry / notice.name)
        inventory.append({"name": package["name"], "version": package["version"], "source": package["source"], "license": package["license"], "repository": package["repository"]})
    (destination / "inventory.json").write_text(json.dumps({"target": host, "dependencies": inventory}, indent=2) + "\n")
    (destination / "README.txt").write_text(
        "Third-party notices for the host-resolved Cargo dependency graph, including build dependencies.\n"
        "Each subdirectory preserves license/copyright notices supplied with that crate.\n"
        "inventory.json records the versions and declared licenses. Fonts are not bundled.\n"
        "These dependency licenses do not change Velnopt's all-rights-reserved policy.\n"
    )
    print(f"Collected notices for {len(inventory)} dependencies ({host}).")


if __name__ == "__main__":
    main()
