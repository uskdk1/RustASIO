#!/usr/bin/env bash
# Run a Windows command in a Proton prefix, inside the Steam Linux Runtime
# container, the same way Steam launches games.
#   PFX=<compatdata dir> scripts/proton-run.sh <command> [args...]
# PFX may also be given as APPID=<steam appid>.
set -euo pipefail
STEAM_ROOT=${STEAM_ROOT:-$HOME/.steam/steam}
PROTON=${PROTON:-"$STEAM_ROOT/steamapps/common/Proton - Experimental"}
SLR=${SLR:-"$STEAM_ROOT/steamapps/common/SteamLinuxRuntime_4"}

if [[ -n "${APPID:-}" ]]; then
    PFX="$STEAM_ROOT/steamapps/compatdata/$APPID"
fi
: "${PFX:?set PFX=<compatdata dir> or APPID=<appid>}"
mkdir -p "$PFX"

export STEAM_COMPAT_DATA_PATH="$PFX"
export STEAM_COMPAT_CLIENT_INSTALL_PATH="$STEAM_ROOT"
export SteamAppId=${APPID:-0} SteamGameId=${APPID:-0}

runner=()
if command -v steam-run >/dev/null 2>&1 && [[ ! -e /etc/NIXOS_FHS ]] && [[ -z "${STEAM_RUNTIME:-}" ]]; then
    runner=(steam-run)   # NixOS: provide an FHS environment for pressure-vessel
fi
exec "${runner[@]}" "$SLR/_v2-entry-point" --verb=waitforexitandrun -- \
    "$PROTON/proton" waitforexitandrun "$@"
