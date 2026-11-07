"""Writes rust-libraries.txt: every Rust library built into cannotcut, with its licence and
the copyright lines from its own licence files, then the full text of each licence once.

Run from anywhere after the dependencies change (it reads `cargo metadata` and the sources
cargo has downloaded):

    python crates/cannotcut-app/licences/rust_libraries.py
"""

import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "rust-libraries.txt")
TARGET = "x86_64-pc-windows-msvc"
LICENCE_FILE = re.compile(r"^(licen[cs]e|copying|notice)", re.IGNORECASE)
# "Copyright 2016 Someone", "(c) 2015 Someone", "© Someone"; not the licences' own sentences
# about copyright notices.
COPYRIGHT = re.compile(
    r"^(copyright\b(?!\s+(notice|licen[cs]e|owner|holder|and\b|statement))|\(c\)\s*\d|©)",
    re.IGNORECASE,
)
# Placeholders in the licence templates, not real notices.
TEMPLATE = re.compile(r"\[yyyy\]|\[name of copyright owner\]|<year>|<copyright holders>")
# Full texts, from the first library found carrying each; the key is the SPDX name.
STANDARD = {
    "MIT": re.compile(r"^licen[cs]e[-_.]?mit", re.IGNORECASE),
    "Apache-2.0": re.compile(r"^licen[cs]e[-_.]?apache", re.IGNORECASE),
    "Zlib": re.compile(r"^licen[cs]e[-_.]?zlib", re.IGNORECASE),
    "BSD-3-Clause": re.compile(r"^licen[cs]e[-_.]?bsd", re.IGNORECASE),
}


def metadata():
    command = ["cargo", "metadata", "--format-version", "1", "--filter-platform", TARGET]
    return json.loads(subprocess.run(command, capture_output=True, check=True).stdout)


def reachable(meta, name):
    """The libraries `name` is built from: its normal dependencies, all the way down."""
    nodes = {node["id"]: node for node in meta["resolve"]["nodes"]}
    root = next(p["id"] for p in meta["packages"] if p["name"] == name)
    seen, stack = set(), [root]
    while stack:
        package = stack.pop()
        if package in seen:
            continue
        seen.add(package)
        for dep in nodes[package]["deps"]:
            if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    return seen


def read(path):
    with open(path, encoding="utf-8", errors="replace") as file:
        return file.read()


def main():
    meta = metadata()
    packages = {p["id"]: p for p in meta["packages"]}
    libraries = sorted(
        (packages[i] for i in reachable(meta, "cannotcut-app") if packages[i]["source"]),
        key=lambda p: (p["name"].lower(), p["version"]),
    )
    standard_texts = {}
    lines = [
        "Rust libraries built into cannotcut",
        "",
        f"{len(libraries)} libraries, each under the licence named beside it (where a choice",
        "is offered, any one of them). Their full texts follow the list.",
        "",
    ]
    for package in libraries:
        folder = os.path.dirname(package["manifest_path"])
        files = sorted(f for f in os.listdir(folder) if LICENCE_FILE.match(f))
        notices = []
        for name in files:
            text = read(os.path.join(folder, name))
            for kind, pattern in STANDARD.items():
                if kind not in standard_texts and pattern.match(name):
                    standard_texts[kind] = text.strip()
            for line in text.splitlines():
                line = line.strip()
                if COPYRIGHT.match(line) and not TEMPLATE.search(line) and line not in notices:
                    notices.append(line)
        licence = package["license"] or "see its own licence file"
        lines.append(f"{package['name']} {package['version']}: {licence}")
        if package.get("repository"):
            lines.append(f"    {package['repository']}")
        lines.extend(f"    {notice}" for notice in notices)
    for kind in STANDARD:
        if kind in standard_texts:
            lines += ["", "", "=" * 72, kind, "=" * 72, "", standard_texts[kind]]
    with open(OUT, "w", encoding="utf-8", newline="\n") as file:
        file.write("\n".join(lines) + "\n")
    print(f"wrote {OUT}: {len(libraries)} libraries, texts: {', '.join(standard_texts)}")


if __name__ == "__main__":
    sys.exit(main())
