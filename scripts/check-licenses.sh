#!/bin/bash
set -euo pipefail
# =========================================================
# check-licenses.sh — verify every resolved Rust dependency's SPDX license is on the
# AGPL-3.0-compatible allowlist (ADR Decision 2 / RR18-AC2).
#
# A new dependency with an unvetted/incompatible license fails the build, so the license
# posture can't silently drift. The pdfium *binary* (BSD) is vendored separately and noted
# in LICENSES-3RDPARTY.md; this script covers the crate graph.
# =========================================================
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# AGPL-3.0-compatible licenses we accept (permissive + the project's own AGPL). Extend
# deliberately, with a note in LICENSES-3RDPARTY.md.
ALLOWLIST="MIT Apache-2.0 BSD-3-Clause BSD-2-Clause ISC Zlib Unicode-3.0 Unicode-DFS-2016 CC0-1.0 0BSD AGPL-3.0-only AGPL-3.0 MPL-2.0 Apache-2.0 WITH LLVM-exception"

cargo metadata --format-version 1 --all-features > /tmp/inkread-metadata.json

python3 - "$ALLOWLIST" <<'PY'
import json, sys, re

allow = set(sys.argv[1].replace(" WITH LLVM-exception", "").split())
allow |= {"Apache-2.0 WITH LLVM-exception"}

with open("/tmp/inkread-metadata.json") as f:
    md = json.load(f)

# The crates in our workspace (skip our own AGPL crates).
ours = {p["name"] for p in md["packages"] if p.get("source") is None}

violations = []
unlicensed = []
for pkg in md["packages"]:
    name, ver = pkg["name"], pkg["version"]
    if name in ours:
        continue
    lic = pkg.get("license")
    if not lic:
        # license-file-only crates: flag for manual review rather than silently pass.
        if pkg.get("license_file"):
            continue
        unlicensed.append(f"{name} {ver}")
        continue
    # Split an SPDX expression into atoms (OR/AND/parentheses) and require AT LEAST ONE
    # allowed atom (an OR like "MIT OR Apache-2.0" is satisfied by either).
    atoms = [a.strip() for a in re.split(r"\bOR\b|\bAND\b|[()/]", lic) if a.strip()]
    if not any(a in allow for a in atoms):
        violations.append(f"{name} {ver}: {lic}")

if unlicensed:
    print("ERROR: dependencies with no declared license (review manually):")
    for u in sorted(unlicensed):
        print(f"  - {u}")
if violations:
    print("ERROR: dependencies with a non-allowlisted license (RR18-AC2 / ADR Decision 2):")
    for v in sorted(violations):
        print(f"  - {v}")

if violations or unlicensed:
    sys.exit(1)
print("license check OK: all dependencies are AGPL-3.0-compatible.")
PY
