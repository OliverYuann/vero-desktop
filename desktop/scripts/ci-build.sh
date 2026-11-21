#!/usr/bin/env bash
# CI build wrapper for the desktop app: signs ONLY what has secrets, and an
# unsigned build still works when none exist. Usage (from desktop/):
#   bash scripts/ci-build.sh --bundles app,dmg        (desktop.yml)
#   bash scripts/ci-build.sh --target universal-apple-darwin   (desktop-build.yml)
#
# The workflows pass every secret under a VERO_-prefixed name. Tauri never
# reads those, so an unset secret (an EMPTY string in Actions) cannot make the
# bundler try to import an empty certificate or notarise with empty
# credentials — it reads APPLE_CERTIFICATE & co. as "set" even when empty.
# This script exports the real names only when a whole group is present:
#
#   macOS signing      APPLE_CERTIFICATE, APPLE_CERTIFICATE_PASSWORD, APPLE_SIGNING_IDENTITY
#   macOS notarisation APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID   (needs signing)
#   Windows signing    WINDOWS_CERT_THUMBPRINT, set by the workflow's import
#                      step from WINDOWS_CERTIFICATE + _PASSWORD
#   Updater            TAURI_SIGNING_PRIVATE_KEY (+ _PASSWORD) AND a real public
#                      key in src-tauri/tauri.updater.conf.json
#
# It prints which groups are ON/OFF (never a value). No secret is written to
# disk: the only file it writes is a config overlay with the signing IDENTITY
# name / certificate thumbprint and the updater settings (none are secrets).
set -euo pipefail
cd "$(dirname "$0")/.."

have() { [ -n "${!1:-}" ]; }
overlay="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/vero-ci.conf.json"
mac_identity=""
win_thumb="${WINDOWS_CERT_THUMBPRINT:-}"
updater=false
extra=()

if have VERO_APPLE_CERTIFICATE && have VERO_APPLE_CERTIFICATE_PASSWORD && have VERO_APPLE_SIGNING_IDENTITY; then
  export APPLE_CERTIFICATE="$VERO_APPLE_CERTIFICATE"
  export APPLE_CERTIFICATE_PASSWORD="$VERO_APPLE_CERTIFICATE_PASSWORD"
  export APPLE_SIGNING_IDENTITY="$VERO_APPLE_SIGNING_IDENTITY"
  mac_identity="$VERO_APPLE_SIGNING_IDENTITY"
  echo "macOS signing: ON"
  if have VERO_APPLE_ID && have VERO_APPLE_PASSWORD && have VERO_APPLE_TEAM_ID; then
    export APPLE_ID="$VERO_APPLE_ID" APPLE_PASSWORD="$VERO_APPLE_PASSWORD" APPLE_TEAM_ID="$VERO_APPLE_TEAM_ID"
    echo "macOS notarisation: ON"
  else
    echo "macOS notarisation: OFF (APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID not all set)"
  fi
else
  echo "macOS signing: OFF (APPLE_CERTIFICATE / _PASSWORD / APPLE_SIGNING_IDENTITY not all set) — ad-hoc, unsigned"
fi

if [ -n "$win_thumb" ]; then
  echo "Windows signing: ON (certificate imported by the workflow)"
else
  echo "Windows signing: OFF (no WINDOWS_CERTIFICATE secret) — unsigned"
fi

if have VERO_TAURI_SIGNING_PRIVATE_KEY; then
  if grep -q "REPLACE_WITH_TAURI_UPDATER_PUBLIC_KEY" src-tauri/tauri.updater.conf.json; then
    echo "::warning::TAURI_SIGNING_PRIVATE_KEY is set but src-tauri/tauri.updater.conf.json still has the placeholder public key. Auto-update stays OFF (SIGNING.md §3)."
  else
    export TAURI_SIGNING_PRIVATE_KEY="$VERO_TAURI_SIGNING_PRIVATE_KEY"
    export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${VERO_TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
    updater=true
    extra+=(--features updater)
    echo "Auto-update: ON (updater artifacts + latest.json signatures)"
  fi
else
  echo "Auto-update: OFF (no TAURI_SIGNING_PRIVATE_KEY secret)"
fi

MAC_IDENTITY="$mac_identity" WIN_THUMB="$win_thumb" UPDATER="$updater" node -e '
  const fs = require("fs");
  const out = {};
  const merge = (a, b) => { for (const [k, v] of Object.entries(b)) a[k] = v && typeof v === "object" && !Array.isArray(v) ? merge(a[k] || {}, v) : v; return a; };
  if (process.env.UPDATER === "true") {
    const u = JSON.parse(fs.readFileSync("src-tauri/tauri.updater.conf.json", "utf8"));
    delete u.$schema;
    merge(out, u);
  }
  if (process.env.MAC_IDENTITY) merge(out, { bundle: { macOS: { signingIdentity: process.env.MAC_IDENTITY } } });
  if (process.env.WIN_THUMB) merge(out, { bundle: { windows: { certificateThumbprint: process.env.WIN_THUMB, digestAlgorithm: "sha256", timestampUrl: "http://timestamp.digicert.com" } } });
  fs.writeFileSync(process.argv[1], JSON.stringify(out, null, 2));
' "$overlay"

if [ "$(cat "$overlay")" != "{}" ]; then
  extra+=(--config "$overlay")
fi

set -x
npx tauri build "$@" ${extra[@]+"${extra[@]}"}
