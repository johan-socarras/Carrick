"""Writes THIRD-PARTY-LICENSES.txt: every crate that goes into Carrick's
programs (normal dependencies, not those for tests or builds), with its
licence and the licence and notice files it ships, for the installers.

    python packaging/third_party.py <output file>
"""
import json
import re
import subprocess
import sys
from pathlib import Path

repo = Path(__file__).resolve().parent.parent
meta = json.loads(subprocess.run(
    ["cargo", "metadata", "--format-version", "1", "--locked"],
    cwd=repo, check=True, capture_output=True, text=True, encoding="utf-8").stdout)
packages = {p["id"]: p for p in meta["packages"]}
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
members = set(meta["workspace_members"])

seen, todo = set(), list(members)
while todo:
    pid = todo.pop()
    if pid in seen:
        continue
    seen.add(pid)
    for dep in nodes[pid]["deps"]:
        if any(k["kind"] is None for k in dep["dep_kinds"]):
            todo.append(dep["pkg"])

# Any file that names a licence or notice, anywhere in its name (such as
# fonts/emoji-icon-font-mit-license.txt), and the font licences OFL and UFL.
NAMES = re.compile(r"(licen[cs]e|copying|notice|copyright)|^(ofl|ufl)\b", re.I)


def licence_files(pkg):
    root = Path(pkg["manifest_path"]).parent
    found = []
    for path in sorted(root.rglob("*")):
        rel = path.relative_to(root)
        if len(rel.parts) > 3 or not path.is_file() or "target" in rel.parts:
            continue
        if NAMES.search(path.name) and path.suffix.lower() in ("", ".txt", ".md", ".mit", ".apache", ".bsd"):
            found.append(path)
        elif path.name.lower().startswith("license") or path.name.lower().startswith("licence"):
            found.append(path)
    if pkg.get("license_file"):
        extra = root / pkg["license_file"]
        if extra.is_file() and extra not in found:
            found.append(extra)
    return root, found


out = [
    "Carrick includes the following third-party software and data.",
    "",
    "=" * 78,
    "EFF Large Wordlist for Passphrases",
    "By the Electronic Frontier Foundation: https://www.eff.org/dice",
    "Licence: Creative Commons Attribution 3.0 United States (CC BY 3.0 US),",
    "https://creativecommons.org/licenses/by/3.0/us/",
    "Changes: Carrick uses the list without its four hyphenated words, to make",
    "the words of its transfer codes.",
    "",
    "Each entry below gives the crate, its version, its licence, and the licence",
    "and notice files it ships.",
    "",
]
shown = {}  # text -> where it was first written out, to give each text once
crates = sorted((packages[p] for p in seen if p not in members), key=lambda p: (p["name"], p["version"]))
for pkg in crates:
    root, files = licence_files(pkg)
    out.append("=" * 78)
    out.append(f'{pkg["name"]} {pkg["version"]}')
    out.append(f'Licence: {pkg.get("license") or "see the files below"}')
    if pkg.get("repository"):
        out.append(f'Source: {pkg["repository"]}')
    for f in files:
        out.append("-" * 78)
        text = f.read_text(encoding="utf-8", errors="replace").rstrip()
        here = f"{pkg['name']} {pkg['version']}, {f.relative_to(root).as_posix()}"
        key = " ".join(text.split())
        if key in shown:
            out.append(f"{f.relative_to(root).as_posix()}: the same text as {shown[key]}, above.")
            continue
        shown[key] = here
        out.append(f"{f.relative_to(root).as_posix()}:")
        out.append("")
        out.append(text)
    out.append("")
Path(sys.argv[1]).write_text("\n".join(out) + "\n", encoding="utf-8", newline="\n")
print(f"{len(crates)} crates written to {sys.argv[1]}")
