#!/bin/bash
set -euo pipefail
# =========================================================
# check-vendor-neutral.sh — enforce IR-7: the Rust core never names a vendor.
#
# `reader-core` and `device-eink` speak capabilities and RefreshIntent/RefreshCommand. All
# device, EPD, and pen specifics belong in the Kotlin adapter and the JNI bridge, so a vendor
# name appearing in either crate is a design smell before it is a naming one — it means a
# device assumption has been written into the portable half.
#
# This started as a cleanup: `DeviceCapabilities::supernote_baseline` / `supernote_full` and
# `ResourceBudget::default_supernote` had shipped vendor names on the core's public API, and a
# dozen comments had encoded "Supernote" where the capability word was the real reason. Nothing
# stopped them coming back, so the invariant is now checked instead of remembered.
#
# A genuinely necessary mention — a citation to an annex ADR whose *filename* carries the name,
# or a clean-room attribution required by RR18 — is exempted by putting `IR-7-ALLOW` on the
# line. That keeps every exemption visible in the diff and greppable in review, rather than
# hidden in a list that lives somewhere else.
#
# Scope note: the other crates are not scanned. `inkread-ink` must name Boox NeoReader to
# attribute the lasso behaviour it reimplements clean-room, and `inkread-update` explains why a
# sideloaded app self-updates. Those are correct, and IR-7 as written in Cargo.toml binds the
# core.
# =========================================================
cd "$(dirname "${BASH_SOURCE[0]}")/.."

CRATES=(reader-core/src device-eink/src device-eink/tests)
# Word-boundaried: without \b, "remarkable" matches inside "unremarkable", which appears in
# the reflow test prose.
VENDORS='\b(supernote|ratta|onyx|boox|remarkable|kindle|kobo|rockchip|rk3566)\b'

hits=$(grep -rniE "$VENDORS" --include='*.rs' "${CRATES[@]}" 2>/dev/null | grep -v 'IR-7-ALLOW' || true)

if [ -n "$hits" ]; then
    echo "IR-7 violation: the Rust core must not name a vendor." >&2
    echo >&2
    echo "$hits" >&2
    echo >&2
    echo "Say what the device *can do* (a capability) instead of who made it. If the mention is" >&2
    echo "genuinely required — an annex-ADR citation, a clean-room attribution — add IR-7-ALLOW" >&2
    echo "to the line with a word on why." >&2
    exit 1
fi

echo "IR-7 OK: no vendor names in ${CRATES[*]}"
