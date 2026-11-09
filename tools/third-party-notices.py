#!/usr/bin/env python3
"""Writes web/public/THIRD-PARTY-NOTICES.txt: Sentry USB's own licence and
NOTICE, then the licences of the npm packages bundled into the web UI and of
the Rust crates built into the Pi binaries. The web UI is embedded in the
sentryusb binary, so the file is served at /THIRD-PARTY-NOTICES.txt
(Settings > System > Resources links to it).

    python3 -I tools/third-party-notices.py            # write the file
    python3 -I tools/third-party-notices.py --check    # fail if it's stale

Crates are those reached through normal dependencies (not build or dev ones)
from the binaries the release ships (PACKAGES), for each Pi target. Their
licence files come from cargo's download of each crate, so run
`cargo fetch --locked` first. npm packages are the production dependencies
in web/package-lock.json, less type-only @types packages, plus the build
tools whose own code lands in web/dist (BUILD_TOOLS). Their licence files
come from web/node_modules, so run `npm ci` in web/ first.

Adapted from Sentry Studio's tools/third-party-notices.py. To keep it
readable:
- a package offered under a choice of licences is used under one of them
  (MIT where offered, see PREFERRED), so only that licence is given;
- an MIT or Apache-2.0 text with the standard wording is reduced to its
  copyright lines and a pointer to one full copy of that standard text;
- any other text seen before (ignoring spacing) is referred to, not repeated.

Packages are named without their versions, so a version-only dependency
bump (most Dependabot PRs) leaves the file unchanged and --check passes.
Adding or removing a package, or a changed licence text, still needs a
regenerated file.
"""
import hashlib
import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "web/public/THIRD-PARTY-NOTICES.txt")
PRODUCT = "Sentry USB"
# The workspace packages whose binaries .github/workflows/build.yml releases.
PACKAGES = ["sentryusb", "sentryusb-tesla-telemetry", "sentryusb-tesla-ble"]
TARGETS = ["aarch64-unknown-linux-gnu", "armv7-unknown-linux-gnueabihf"]
WEB = os.path.join(ROOT, "web")
# Dev dependencies whose own code is in web/dist: Vite's preload helper,
# Rolldown's module runtime and Tailwind's base styles.
BUILD_TOOLS = ["vite", "rolldown", "tailwindcss"]
# Where a build tool's licence file moves on to the code bundled into the
# tool's own Node.js build, which is not in web/dist.
TOOL_LICENCE_END = {"vite": "# Licenses of bundled dependencies"}
FONTS = [
    ("Inter", "SIL Open Font License 1.1", "web/public/fonts/Inter-OFL.txt"),
    ("JetBrains Mono", "SIL Open Font License 1.1", "web/public/fonts/JetBrainsMono-OFL.txt"),
]
# Other components NOTICE lists, with the standard licence text that covers them.
NOTICE_EXTRAS = [
    ("Material Symbols icons (NOTICE section 5)", "Apache-2.0"),
]
APACHE_FILE = os.path.join(ROOT, "tools/licenses/Apache-2.0.txt")
LICENCE_PREFIXES = ("license", "licence", "copying", "notice", "unlicense", "copyright")
# Licences that would not sit with a noncommercial binary distribution, or
# that need their source offered; flagged when found.
FLAG = re.compile(r"\b(A?GPL|LGPL|MPL|EUPL|CDDL|SSPL)", re.I)
with open(APACHE_FILE, encoding="utf-8") as _f:
    APACHE_WORDS = " ".join(re.findall(r"[a-z0-9]+", _f.read().lower())).split("end of terms and conditions")[0].strip()
RULE = "=" * 78
# For packages published without a licence file: the standard texts, under
# their authors' names. An "OR" expression is covered by its first licence
# with a standard text here.
MIT = """Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE."""
BOOST = """Boost Software License - Version 1.0 - August 17th, 2003

Permission is hereby granted, free of charge, to any person or organization
obtaining a copy of the software and accompanying documentation covered by
this license (the "Software") to use, reproduce, display, distribute,
execute, and transmit the Software, and to prepare derivative works of the
Software, and to permit third-parties to whom the Software is furnished to
do so, all subject to the following:

The copyright notices in the Software and this entire statement, including
the above license grant, this restriction and the following disclaimer,
must be included in all copies of the Software, in whole or in part, and
all derivative works of the Software, unless such copies or derivative
works are solely in the form of machine-executable object code generated by
a source language processor.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE, TITLE AND NON-INFRINGEMENT. IN NO EVENT
SHALL THE COPYRIGHT HOLDERS OR ANYONE DISTRIBUTING THE SOFTWARE BE LIABLE
FOR ANY DAMAGES OR OTHER LIABILITY, WHETHER IN CONTRACT, TORT OR OTHERWISE,
ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE."""
# Packages published without a licence file whose licence is not one of the
# standard texts: the sibling, from the same repository and authors, whose
# licence file covers them.
SAME_AS = {"alloc-stdlib": "alloc-no-stdlib"}
# Licence id -> where its standard text is in the file.
STANDARD = {
    "MIT": "Standard licence texts: MIT",
    "Apache-2.0": "Standard licence texts: Apache-2.0",
    "BSL-1.0": "Standard licence texts: BSL-1.0",
    "CC0-1.0": None,
}


def crates():
    """{id: package} for every crate a shipped binary links on any target."""
    found = {}
    for target in TARGETS:
        meta = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target], cwd=ROOT))
        packages = {p["id"]: p for p in meta["packages"]}
        nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
        members = set(meta["workspace_members"])
        roots = [i for i in members if packages[i]["name"] in PACKAGES]
        if len(roots) != len(PACKAGES):
            sys.exit(f"workspace packages not found: {sorted(set(PACKAGES) - {packages[i]['name'] for i in roots})}")
        stack, seen = list(roots), set(roots)
        while stack:
            for dep in nodes[stack.pop()]["deps"]:
                if any(k["kind"] is None for k in dep["dep_kinds"]) and dep["pkg"] not in seen:
                    seen.add(dep["pkg"])
                    stack.append(dep["pkg"])
        for pid in seen - members:
            found[pid] = packages[pid]
    return found


def repo_url(manifest):
    """A browsable URL from a package.json's repository (or homepage)."""
    repo = manifest.get("repository")
    url = repo.get("url", "") if isinstance(repo, dict) else (repo or "")
    if url:
        url = re.sub(r"^git\+", "", url)
        url = re.sub(r"^git://", "https://", url)
        url = re.sub(r"^(ssh://)?git@github\.com[:/]", "https://github.com/", url)
        url = re.sub(r"^github:", "https://github.com/", url)
        if re.fullmatch(r"[\w.-]+/[\w.-]+", url):
            url = "https://github.com/" + url
        url = re.sub(r"\.git$", "", url)
        return url
    return manifest.get("homepage") or ""


def npm_packages():
    """[package] for each npm package in web/dist, shaped like cargo's."""
    with open(os.path.join(WEB, "package-lock.json"), encoding="utf-8") as f:
        locked = json.load(f)["packages"]
    keys = [k for k, v in locked.items() if k and not v.get("dev") and not v.get("devOptional") and not v.get("optional")]
    keys = [k for k in keys if not k.split("node_modules/")[-1].startswith("@types/")]
    keys += [f"node_modules/{n}" for n in BUILD_TOOLS if f"node_modules/{n}" not in keys]
    found = []
    for key in keys:
        folder = os.path.join(WEB, key)
        manifest_path = os.path.join(folder, "package.json")
        if not os.path.isfile(manifest_path):
            sys.exit(f"{key} is not installed: run npm ci in web/")
        with open(manifest_path, encoding="utf-8") as f:
            manifest = json.load(f)
        if manifest.get("version") != locked[key].get("version"):
            sys.exit(f"{key} is {manifest.get('version')}, not the locked {locked[key].get('version')}: run npm ci in web/")
        licence = manifest.get("license") or locked[key].get("license")
        if isinstance(licence, dict):
            licence = licence.get("type")
        author = manifest.get("author")
        if isinstance(author, dict):
            author = author.get("name")
        found.append({
            "name": manifest.get("name") or key.split("node_modules/")[-1],
            "version": manifest["version"],
            "license": licence if isinstance(licence, str) else None,
            "repository": repo_url(manifest),
            "authors": [author] if isinstance(author, str) else [],
            "manifest_path": manifest_path,
        })
    return found


def words(text):
    """A text's words in order, for comparing licence texts whatever their
    spacing and line breaks."""
    return " ".join(re.findall(r"[a-z0-9]+", text.lower()))


MIT_WORDS = words(MIT)
APACHE_END = "end of terms and conditions"


def standard_kind(text):
    """("MIT" or "Apache-2.0", the text's own lines before the standard
    wording), when the text is that licence's standard wording; else None."""
    w = words(text)
    if w.endswith(MIT_WORDS) and w.count("permission is hereby granted free of charge") == 1:
        head = text[: text.lower().find("permission is hereby granted")]
        return "MIT", head
    if "apache license version 2 0 january 2004" in w and APACHE_END in w and w.split(APACHE_END)[0].strip() == APACHE_WORDS:
        # The appendix after the terms is the template, unless it was filled in.
        tail = text[text.upper().find("END OF TERMS AND CONDITIONS") :]
        notices = [l.strip() for l in tail.splitlines() if re.match(r"\s*copyright\b", l, re.I) and "[yyyy]" not in l.lower()]
        return "Apache-2.0", "\n".join(notices)
    return None


# The licence used when a package offers a choice ("A OR B"), first first.
PREFERRED = ["MIT", "Apache-2.0", "Zlib", "BSD-3-Clause", "BSD-2-Clause", "ISC", "CC0-1.0"]
# How a licence file's name or text shows which licence it is.
FILE_TOKENS = {"MIT": "mit", "Apache-2.0": "apache", "Zlib": "zlib", "BSD-3-Clause": "bsd", "BSD-2-Clause": "bsd", "ISC": "isc", "CC0-1.0": "cc0"}


def chosen_files(package, files):
    """The licence files that apply. A package offered under a choice of
    licences (and no AND) is used under the first of PREFERRED it offers, so
    only that licence's files are given, with any COPYRIGHT or NOTICE file."""
    expr = (package.get("license") or "").replace("/", " OR ")
    offered = [i.strip("() ") for i in expr.split(" OR ")]
    if " AND " in expr or len(offered) < 2:
        return files
    chosen = next((i for i in PREFERRED if i in offered), None)
    if chosen is None:
        return files

    def is_chosen(name, path):
        if FILE_TOKENS[chosen] in name.lower():
            return True
        with open(path, encoding="utf-8", errors="replace") as f:
            kind = standard_kind(f.read())
        return kind is not None and kind[0] == chosen

    picked = [(n, p) for n, p in files if is_chosen(n, p)]
    notices = [(n, p) for n, p in files if n.lower().startswith(("copyright", "notice")) and (n, p) not in picked]
    return picked + notices if picked else files


def licence_files(package):
    folder = os.path.dirname(package["manifest_path"])
    names = sorted(n for n in os.listdir(folder) if n.lower().startswith(LICENCE_PREFIXES) and os.path.isfile(os.path.join(folder, n)))
    if package.get("license_file"):
        extra = os.path.relpath(os.path.join(folder, package["license_file"]), folder)
        if extra not in names and os.path.isfile(os.path.join(folder, extra)):
            names.append(extra)
    if not names:
        # A package that vendors others may carry only their licence files,
        # in subfolders (victory-vendor's lib-vendor/d3-*/LICENSE).
        for here, dirs, files in os.walk(folder):
            dirs[:] = sorted(d for d in dirs if d != "node_modules")
            names += sorted(os.path.relpath(os.path.join(here, n), folder) for n in files if here != folder and n.lower().startswith(LICENCE_PREFIXES))
    return [(n, os.path.join(folder, n)) for n in names]


def read(path):
    with open(os.path.join(ROOT, path), encoding="utf-8") as f:
        return f.read().rstrip()


def version_series(version):
    """The semver-compatible series: 2.0.21 -> 2.x, 0.6.4 -> 0.6.x."""
    parts = re.split(r"[.+-]", version)
    if parts[0] != "0":
        return f"{parts[0]}.x"
    if len(parts) > 1 and parts[1] != "0":
        return f"0.{parts[1]}.x"
    return version


def package_labels(packages):
    """Names without versions, so a version-only dependency bump doesn't
    change the file (Dependabot can't regenerate it). A name present in
    several versions gets its version series, or the full version if two
    share a series."""
    by_name = {}
    for p in packages:
        by_name.setdefault(p["name"], []).append(p)
    labels = {}
    for name, group in by_name.items():
        series = [version_series(p["version"]) for p in group]
        for p, s in zip(group, series):
            if len(group) == 1:
                labels[id(p)] = name
            elif series.count(s) == 1:
                labels[id(p)] = f"{name} {s}"
            else:
                labels[id(p)] = f'{name} {p["version"]}'
    return labels


def render():
    out = [
        f"{PRODUCT}: licences",
        "",
        f"{PRODUCT} is under the PolyForm Noncommercial License 1.0.0 below,",
        "except for the files its NOTICE lists. It is built with the open-source",
        "fonts, npm packages and Rust crates after those, each under its own",
        "licence, reproduced here. Generated by tools/third-party-notices.py.",
        "",
        RULE,
        f"{PRODUCT}: LICENSE",
        RULE,
        "",
        read("LICENSE"),
        "",
        RULE,
        f"{PRODUCT}: NOTICE",
        RULE,
        "",
        read("NOTICE"),
        "",
    ]
    printed = {}
    missing = []
    flagged = []

    def text_block(title, text, shorten=True):
        kind = standard_kind(text) if shorten else None
        if kind:
            name, head = kind
            head = "\n".join(l.rstrip() for l in head.strip().splitlines() if l.strip() and words(l) not in ("mit license", "the mit license mit", "mit"))
            if head:
                out.append(head)
            out.append(f'{name}: the standard text in "{STANDARD[name]}", above.')
            out.append("")
            return
        key = hashlib.sha256(words(text).encode()).hexdigest()
        if key in printed:
            out.append(f"{title}: the same text as {printed[key]}, above.")
        else:
            printed[key] = title
            out.append(f"--- {title} ---")
            out.append(text.rstrip())
        out.append("")

    def section(heading, packages):
        out.extend([RULE, heading, RULE, ""])
        packages = sorted(packages, key=lambda p: (p["name"], p["version"]))
        labels = package_labels(packages)
        for package in packages:
            label = labels[id(package)]
            out.append(f'{label} ({package.get("license") or "see below"})')
            if FLAG.search(package.get("license") or ""):
                flagged.append(f'{label} ({package["license"]})')
            if package.get("repository"):
                out.append(package["repository"])
            files = licence_files(package)
            sibling = next((p for p in packages if p["name"] == SAME_AS.get(package["name"])), None)
            if not files and sibling and licence_files(sibling):
                missing.append(label)
                out.append(f'Published without a licence file; covered by the licence given for {labels[id(sibling)]}, from the same authors and repository.')
                out.append("")
            elif not files or all(os.sep in n for n, _ in files):
                missing.append(label)
                ids = [i.strip("() ") for i in re.split(r" OR | AND ", (package.get("license") or "").replace("/", " OR "))]
                chosen = next((i for i in ids if i in STANDARD), None)
                authors = ", ".join(a.split(" <")[0] for a in package.get("authors", [])) or f'the {package["name"]} authors'
                out.append(f"Copyright (c) {authors}")
                if chosen is None:
                    sys.exit(f"{label}: no standard text for {package.get('license')}; add one to STANDARD")
                elif STANDARD[chosen] is None:
                    out.append(f"Licensed under {chosen}, a public-domain dedication: no notice is required.")
                else:
                    out.append(f'Used under {chosen}; it is published without a licence file, so its terms are the standard text in "{STANDARD[chosen]}", above.')
                out.append("")
            for name, path in chosen_files(package, files):
                with open(path, encoding="utf-8", errors="replace") as f:
                    text = f.read()
                end = TOOL_LICENCE_END.get(package["name"])
                if end and end in text:
                    text = text[: text.index(end)]
                    out.append(f'{label}: its licence file without the part from "{end}" on, which covers code inside the tool, not in the web UI.')
                text_block(f"{label}: {name}", text)

    out += [RULE, "Standard licence texts", RULE, ""]
    text_block("Standard licence texts: MIT", "MIT License\n\nCopyright (c) <the copyright holders named with each package>\n\n" + MIT, shorten=False)
    with open(APACHE_FILE, encoding="utf-8") as f:
        text_block("Standard licence texts: Apache-2.0", f.read(), shorten=False)
    text_block("Standard licence texts: BSL-1.0", BOOST, shorten=False)

    out += [RULE, "Fonts and icons" if NOTICE_EXTRAS else "Fonts", RULE, ""]
    for name, licence, path in FONTS:
        out.append(f"{name} ({licence})")
        text_block(f"{name}: {os.path.basename(path)}", read(path))
    for name, licence in NOTICE_EXTRAS:
        out.append(f"{name} ({licence})")
        out.append(f'{licence}: the standard text in "{STANDARD[licence]}", above.')
        out.append("")

    section("Web UI: npm packages", npm_packages())
    section("Pi binaries: Rust crates", crates().values())
    return "\n".join(out).rstrip() + "\n", missing, flagged


def main():
    text, missing, flagged = render()
    if "--check" in sys.argv:
        current = open(OUT, encoding="utf-8").read() if os.path.exists(OUT) else ""
        if current != text:
            sys.exit(f"{os.path.relpath(OUT, ROOT)} is out of date: run python3 -I tools/third-party-notices.py")
        return
    with open(OUT, "w", encoding="utf-8") as f:
        f.write(text)
    print(f"wrote {OUT} ({len(text) // 1024} KB)")
    if missing:
        print(f"{len(missing)} packages have no licence file; the standard texts cover them: {', '.join(missing)}")
    if flagged:
        print(f"check these licences: {', '.join(flagged)}")


if __name__ == "__main__":
    main()
