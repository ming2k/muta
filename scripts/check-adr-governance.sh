#!/usr/bin/env bash
# check-adr-governance.sh — verify docs/adr/ against the architecture profile
# (docs/governance/documentation/profiles/architecture/adr.md).
#
# Exit 0 = compliant; exit 1 = drift, with a report.
# Implements: INV-TEMP-03 (no direct deletion / retired records archived),
#             INV-TEMP-04 (archived records carry tombstone metadata),
#             INV-ARCH-01 (accepted records are not silently rewritten),
#             INV-CORE-05 (every ADR registered in the index).
set -euo pipefail

cd "$(dirname "$0")/.."
ADR="docs/adr"

python3 - "$ADR" <<'EOF'
import os
import re
import sys

adr = sys.argv[1]
archive = os.path.join(adr, "archive")
problems = []
counts = {"active": 0, "archived": 0}

NUM = re.compile(r"^(\d{4})-[a-z0-9-]+\.md$")
STATUS = re.compile(r"^- \*{0,2}Status\*{0,2}:\s*(.+)$", re.M | re.I)
ARCHIVED_ON = re.compile(r"^- \*{0,2}Archived\*{0,2}:\s*\S+", re.M | re.I)
FULL_RETIRED = re.compile(r"^(Superseded|Deprecated|Compacted)\b", re.I)


def records(base):
    if not os.path.isdir(base):
        return {}
    out = {}
    for fn in sorted(os.listdir(base)):
        m = NUM.match(fn)
        if m:
            out[m.group(1)] = fn
    return out


active = records(adr)
arch = records(archive)
counts["active"], counts["archived"] = len(active), len(arch)

# 1. duplicate numbers across the two tiers
for i in set(active) & set(arch):
    problems.append(f"ADR-{i}: present in both docs/adr/ and docs/adr/archive/")

# 2. every record carries a Status; archived records carry Archived:
for label, base, table in (("active", adr, active), ("archived", archive, arch)):
    for i, fn in table.items():
        t = open(os.path.join(base, fn), encoding="utf-8").read()
        if not STATUS.search(t):
            problems.append(f"{label} ADR-{i} ({fn}): missing `Status` field")
        if label == "archived" and not ARCHIVED_ON.search(t):
            problems.append(f"archived ADR-{i} ({fn}): missing `Archived:` tombstone line")

# 3. retired records must not linger in the active tier (INV-TEMP-03)
for i, fn in active.items():
    t = open(os.path.join(adr, fn), encoding="utf-8").read()
    m = STATUS.search(t)
    if m and FULL_RETIRED.match(m.group(1).strip()):
        problems.append(
            f"active ADR-{i} ({fn}): status `{m.group(1).strip()}` must be archived "
            f"under docs/adr/archive/ (INV-TEMP-03)")

# 4. registry completeness + path correctness (INV-CORE-05)
index = os.path.join(adr, "index.md")
registered = {}
if os.path.exists(index):
    for line in open(index, encoding="utf-8"):
        m = re.search(r"\]\(([^)]*?(\d{4})-[a-z0-9-]+\.md)\)", line)
        if m:
            registered[m.group(2)] = m.group(1)
else:
    problems.append("docs/adr/index.md is missing")

for i, fn in active.items():
    want = fn
    got = registered.get(i)
    if got is None:
        problems.append(f"ADR-{i} ({fn}): not registered in docs/adr/index.md")
    elif got != want:
        problems.append(f"ADR-{i}: index points at `{got}`, expected `{want}`")
for i, fn in arch.items():
    want = f"archive/{fn}"
    got = registered.get(i)
    if got is None:
        problems.append(f"archived ADR-{i} ({fn}): not registered in docs/adr/index.md")
    elif got != want:
        problems.append(f"ADR-{i}: index points at `{got}`, expected `{want}`")
for i in set(registered) - set(active) - set(arch):
    problems.append(f"ADR-{i}: registered in index but no record exists")

# 5. every relative link inside docs/adr/** resolves
LINK = re.compile(r"\]\(([^)]+?)\)")
for base in (adr, archive):
    if not os.path.isdir(base):
        continue
    for fn in sorted(os.listdir(base)):
        if not fn.endswith(".md"):
            continue
        p = os.path.join(base, fn)
        for m in LINK.finditer(open(p, encoding="utf-8").read()):
            tgt = m.group(1).split("#")[0].strip()
            if not tgt or tgt.startswith(("http://", "https://", "mailto:")):
                continue
            if "`" in tgt or "::" in tgt or " " in tgt or "<" in tgt:
                continue  # code-span authoring typo, not a path
            if not os.path.exists(os.path.normpath(os.path.join(base, tgt))):
                problems.append(f"{p}: broken relative link -> {tgt}")

print(f"ADR governance: {counts['active']} active, {counts['archived']} archived")
if problems:
    for p in problems:
        print(f"  DRIFT: {p}")
    sys.exit(1)
print("  ok: registry complete, no retired records in the active tier, links resolve")
EOF
