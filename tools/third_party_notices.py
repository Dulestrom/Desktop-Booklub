#!/usr/bin/env python3
"""Writes the licence texts of every Rust crate compiled into Booklub, for shipping in the .deb.

Exits non-zero, without writing, if a crate ships no text for the licence it declares. Reads only
what cargo reports and the crates' own files, and writes crate names rather than local paths.

usage: tools/third_party_notices.py
Writes <target>/release/THIRD-PARTY-NOTICES.txt, where cargo-deb picks it up, and prints that path.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT_PACKAGE = "booklub-app"

# How to recognise each licence's text. "Is this file really the licence" matters because some
# crates ship a LICENSE or COPYING that only points at another file.
LICENCE_MARKERS = {
    "0BSD": [r"with or without fee is hereby granted\.\s+THE SOFTWARE"],
    "Apache-2.0": [r"Apache License", r"Version 2\.0"],
    "BSD-3-Clause": [r"Redistribution and use in source and binary forms", r"Neither the name"],
    "ISC": [r"Permission to use, copy, modify, and/or distribute", r"provided that the above"],
    "MIT": [r"Permission is hereby granted, free of charge"],
    "Unicode-3.0": [r"UNICODE LICENSE V3"],
    "Unlicense": [r"This is free and unencumbered software"],
    "Zlib": [r"The origin of this software must not be misrepresented"],
}

LICENCE_FILE = re.compile(r"(un)?licen[cs]e|copying|copyright|notice", re.IGNORECASE)
LICENCE_DIR = re.compile(r"licen[cs]es?", re.IGNORECASE)
NOT_TEXT = {".rs", ".toml", ".json", ".py", ".sh", ".html", ".c", ".h"}


def run(*command):
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout


def host_target():
    return re.search(r"^host: (\S+)$", run("rustc", "-vV"), re.MULTILINE).group(1)


def rust_std_notice():
    # std and the crates it pulls in are linked into the binary but are not in cargo's dependency
    # data, so their notices come from the toolchain's own package.
    version = re.match(r"rustc (\d+\.\d+)", run("rustc", "-V")).group(1)
    path = Path(f"/usr/share/doc/libstd-rust-{version}/copyright")
    if not path.is_file():
        raise FileNotFoundError(
            f"Rust standard library: no {path}. Build with Ubuntu's rustc, as BUILDING.md describes."
        )
    return read_text(path)


def compiled_crates(target):
    # cargo tree rather than the graph in cargo metadata, which unifies features across build and
    # dev dependencies and so lists crates that are never compiled into the app.
    tree = run("cargo", "tree", "--locked", "-p", ROOT_PACKAGE, "-e", "normal", "--target", target,
               "--prefix", "none", "-f", "{p}")
    return {tuple(line.split()[:2]) for line in tree.splitlines() if line.strip()}


def licence_files(crate_dir):
    found = []
    for path in sorted(crate_dir.rglob("*")):
        if not path.is_file() or path.suffix.lower() in NOT_TEXT:
            continue
        relative = path.relative_to(crate_dir)
        if "target" in relative.parts[:-1] or ".git" in relative.parts[:-1]:
            continue
        in_licence_dir = any(LICENCE_DIR.fullmatch(part) for part in relative.parts[:-1])
        if LICENCE_FILE.match(path.name) or in_licence_dir:
            found.append(path)
    return found


def read_text(path):
    raw = path.read_bytes()
    try:
        text = raw.decode("utf-8-sig")
    except UnicodeDecodeError:
        text = raw.decode("latin-1")
    lines = [line.rstrip() for line in text.replace("\r\n", "\n").replace("\r", "\n").split("\n")]
    return "\n".join(lines).strip("\n")


def licences_in(text):
    return {
        licence
        for licence, markers in LICENCE_MARKERS.items()
        if all(re.search(marker, text, re.IGNORECASE) for marker in markers)
    }


def satisfied(expression, available):
    """Evaluates an SPDX expression, or raises ValueError on a licence with no marker above."""
    tokens = re.findall(r"[()/]|[^\s()/]+", expression)

    def parse_or():
        result = parse_and()
        while tokens and tokens[0] in ("OR", "/"):
            tokens.pop(0)
            right = parse_and()
            result = result or right
        return result

    def parse_and():
        result = parse_term()
        while tokens and tokens[0] == "AND":
            tokens.pop(0)
            right = parse_term()
            result = result and right
        return result

    def parse_term():
        token = tokens.pop(0)
        if token == "(":
            result = parse_or()
            tokens.pop(0)
            return result
        if token not in LICENCE_MARKERS:
            raise ValueError(token)
        return token in available

    return parse_or()


def collect(packages, compiled, workspace_members):
    """Returns ({text key: (text, [(crate label, file name)])}, [crate lines], [problems])."""
    groups, index, problems = {}, [], []
    for package in sorted(packages, key=lambda p: (p["name"], p["version"])):
        if (package["name"], f"v{package['version']}") not in compiled:
            continue
        if package["id"] in workspace_members:
            continue
        label = f"{package['name']} {package['version']}"
        expression = package["license"]
        crate_dir = Path(package["manifest_path"]).parent
        files = licence_files(crate_dir)
        if package["license_file"]:
            files = sorted(set(files) | {crate_dir / package["license_file"]})
        texts = {path: read_text(path) for path in files if path.exists()}

        available = set().union(*map(licences_in, texts.values())) if texts else set()
        if not texts:
            problems.append(f"{label}: no licence file (declares {expression})")
        elif expression:
            try:
                if not satisfied(expression, available):
                    found = ", ".join(sorted(available)) or "nothing recognisable"
                    problems.append(f"{label}: declares {expression} but ships {found}")
            except ValueError as unknown:
                problems.append(f"{label}: add a marker for {unknown} to LICENCE_MARKERS")
        elif not package["license_file"]:
            problems.append(f"{label}: declares no licence")

        index.append(f"{label}: {expression or 'see its licence file'}")
        for path, text in texts.items():
            # Grouping ignores how lines wrap, but not any wording or copyright holder.
            key = " ".join(text.split())
            groups.setdefault(key, (text, []))[1].append(
                (label, path.relative_to(crate_dir).as_posix())
            )
    return groups, index, problems


def render(groups, index, own_licence, std_notice):
    rule = "=" * 78
    out = [
        "Third-party notices for Booklub",
        "",
        "This file is generated when the package is built. The binary is built from the Rust",
        "crates below, and this file holds the licence files each one ships. Where a crate",
        "offers a choice of licences, all of its licence files are here. GTK, libadwaita, and",
        "SQLite are linked dynamically from the system and are not included in this package.",
        "",
        f"{rule}\nBooklub\n{rule}",
        "",
        own_licence,
        "",
        "",
        f"{rule}\nCrates ({len(index)})\n{rule}",
        "",
        *index,
    ]
    ordered = sorted(groups.values(), key=lambda group: (-len(group[1]), group[1][0]))
    for text, users in ordered:
        names = sorted({f"{crate} ({file})" for crate, file in users})
        out += ["", "", rule, "Used by:", *[f"  {name}" for name in names], rule, "", text]
    out += [
        "",
        "",
        rule,
        "The Rust standard library",
        "",
        "Part of it is compiled into the binary. This is the copyright file of the toolchain",
        "that built this package, and it also covers parts of Rust that are not included here.",
        rule,
        "",
        std_notice,
    ]
    return "\n".join(out) + "\n"


def main():
    target = host_target()
    metadata = json.loads(
        run("cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target)
    )
    compiled = compiled_crates(target)
    groups, index, problems = collect(
        metadata["packages"], compiled, set(metadata["workspace_members"])
    )
    if problems:
        print("REFUSING: these crates have no usable licence text:", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        sys.exit(1)
    own_licence = read_text(Path(metadata["workspace_root"]) / "LICENSE")
    try:
        std_notice = rust_std_notice()
    except FileNotFoundError as missing:
        sys.exit(f"REFUSING: {missing}")
    out_file = Path(metadata["target_directory"]) / "release" / "THIRD-PARTY-NOTICES.txt"
    out_file.parent.mkdir(parents=True, exist_ok=True)
    out_file.write_text(render(groups, index, own_licence, std_notice), encoding="utf-8")
    print(f"{len(index)} crates, {len(groups)} distinct licence texts.", file=sys.stderr)
    print(out_file)


if __name__ == "__main__":
    main()
