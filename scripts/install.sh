#!/usr/bin/env bash
# Install / uninstall RustASIO into a Steam (Proton) game prefix.
#   scripts/install.sh [APPID]              default APPID 960170 (DJMAX RESPECT V)
#   scripts/install.sh --uninstall [APPID]
# Run `nix develop -c cargo xtask dist` first. The game must not be running.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
STEAM_ROOT=${STEAM_ROOT:-$HOME/.steam/steam}
DEST=${RUSTASIO_DEST:-$HOME/.local/share/rustasio/lib/wine}

uninstall=0
if [[ "${1:-}" == "--uninstall" ]]; then uninstall=1; shift; fi
APPID=${1:-960170}
PFX=${PFX:-"$STEAM_ROOT/steamapps/compatdata/$APPID"}
SYS32="$PFX/pfx/drive_c/windows/system32"

die() { echo "install: $*" >&2; exit 1; }
[[ -d "$SYS32" ]] || die "no Proton prefix at $PFX (run the game once first)"
# Is any Wine process using this prefix? (Match WINEPREFIX in the environment,
# not command lines: the shell running this script may mention the path.)
prefix_in_use() {
    local want p
    want=$(realpath -m "$PFX/pfx")
    for p in /proc/[0-9]*; do
        [[ $p == "/proc/$$" ]] && continue
        local env
        env=$(tr '\0' '\n' < "$p/environ" 2>/dev/null | sed -n 's/^WINEPREFIX=//p') || continue
        [[ -n $env ]] && [[ $(realpath -m "$env") == "$want" ]] && return 0
    done
    return 1
}
if prefix_in_use; then
    die "a Wine process is using $PFX; quit the game first"
fi

if (( uninstall )); then
    WINEDLLPATH="$DEST" PFX="$PFX" "$here/scripts/proton-run.sh" regsvr32 /u /s rustasio64.dll || true
    rm -f "$SYS32/rustasio64.dll"
    echo "RustASIO removed from $PFX (files in $DEST kept)."
    exit 0
fi

src="$here/dist/lib/wine"
[[ -f "$src/x86_64-windows/rustasio64.dll" && -f "$src/x86_64-unix/rustasio64.so" ]] \
    || die "dist/ missing; run: nix develop -c cargo xtask dist"

mkdir -p "$DEST/x86_64-windows" "$DEST/x86_64-unix"
install -m644 "$src/x86_64-windows/rustasio64.dll" "$DEST/x86_64-windows/"
install -m755 "$src/x86_64-unix/rustasio64.so" "$DEST/x86_64-unix/"
install -m644 "$src/x86_64-windows/rustasio64.dll" "$SYS32/"

WINEDLLPATH="$DEST" PFX="$PFX" "$here/scripts/proton-run.sh" regsvr32 /s rustasio64.dll

if grep -q 'Software\\\\ASIO\\\\RustASIO' "$PFX/pfx/system.reg"; then
    echo "RustASIO registered in $PFX."
else
    die "registration did not show up in system.reg"
fi
cat <<MSG

Set the game's Steam launch options to (keep your other variables):
  WINEDLLPATH=$DEST %command%
Optional: RUSTASIO_LOG=1 PROTON_LOG=1 to get [rustasio] lines in ~/steam-$APPID.log
MSG
