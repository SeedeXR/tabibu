#!/bin/zsh
# Honest uninstaller for Tabibu.
#
# Usage:
#   scripts/uninstall-tabibu.sh             dry run: list what WOULD be removed
#   scripts/uninstall-tabibu.sh --dry-run   same as above, explicit
#   scripts/uninstall-tabibu.sh --yes       actually remove everything listed
#
# Removes:
#   /Applications/Tabibu.app
#   ~/Library/Application Support/Tabibu
#   ~/Library/Caches/xr.seede.tabibu
#   ~/Library/Preferences/xr.seede.tabibu.plist
#   launchd agents matching xr.seede.tabibu.* (booted out + plists removed)
set -euo pipefail

MODE="dry-run"
case "${1:-}" in
  "" | --dry-run) MODE="dry-run" ;;
  --yes)          MODE="delete" ;;
  *)
    echo "usage: $0 [--dry-run | --yes]" >&2
    exit 2
    ;;
esac

if [[ "$MODE" == "dry-run" ]]; then
  echo "DRY RUN -- nothing will be deleted. Re-run with --yes to actually remove."
fi

FOUND=0

remove_path() {
  local p="$1"
  if [[ -e "$p" || -L "$p" ]]; then
    FOUND=1
    if [[ "$MODE" == "delete" ]]; then
      rm -rf "$p"
      echo "removed:      $p"
    else
      echo "would remove: $p"
    fi
  fi
}

# --- launchd agents xr.seede.tabibu.* ------------------------------------------
UID_NUM="$(id -u)"
# Boot out any loaded agents first (only in delete mode), then remove plists.
for label in $(launchctl list 2>/dev/null | awk '{print $3}' | grep '^ai\.bsa\.tabibu' || true); do
  FOUND=1
  if [[ "$MODE" == "delete" ]]; then
    if launchctl bootout "gui/$UID_NUM/$label" 2>/dev/null; then
      echo "booted out:   launchd agent $label"
    else
      echo "warning: failed to boot out $label (may already be unloading)" >&2
    fi
  else
    echo "would boot out: launchd agent $label"
  fi
done
for plist in "$HOME/Library/LaunchAgents"/xr.seede.tabibu*.plist(N); do
  remove_path "$plist"
done

# --- Salama encrypted-DNS resolver (root LaunchDaemon) ------------------------
# Restores system DNS FIRST, then unloads + deletes the daemon. Needs root.
DOHD_PLIST="/Library/LaunchDaemons/xr.seede.tabibu.dohd.plist"
DOHD_BIN="/usr/local/libexec/tabibu-dohd"
if [[ -e "$DOHD_PLIST" || -e "$DOHD_BIN" ]]; then
  FOUND=1
  if [[ "$MODE" == "delete" ]]; then
    echo "Removing the Salama resolver and restoring DNS (needs your password)…"
    sudo /bin/sh -c '
      P=/Library/LaunchDaemons/xr.seede.tabibu.dohd.plist
      B=/usr/local/libexec/tabibu-dohd
      /usr/sbin/networksetup -listallnetworkservices | while IFS= read -r svc; do
        case "$svc" in ""|\**|"An asterisk"*) continue;; esac
        /usr/sbin/networksetup -setdnsservers "$svc" empty 2>/dev/null || true
      done
      /bin/launchctl bootout system "$P" 2>/dev/null || true
      /bin/rm -f "$P" "$B"
    '
    echo "removed:      Salama resolver + restored system DNS"
  else
    echo "would remove: Salama resolver ($DOHD_PLIST + $DOHD_BIN) + restore system DNS"
  fi
fi

# --- files --------------------------------------------------------------------
remove_path "/Applications/Tabibu.app"
remove_path "$HOME/Library/Application Support/Tabibu"
remove_path "$HOME/Library/Caches/xr.seede.tabibu"
# Clear the prefs domain via cfprefsd BEFORE removing the plist, otherwise the
# daemon can rewrite it from its in-memory cache.
if [[ "$MODE" == "delete" && -e "$HOME/Library/Preferences/xr.seede.tabibu.plist" ]]; then
  defaults delete xr.seede.tabibu 2>/dev/null || true
fi
remove_path "$HOME/Library/Preferences/xr.seede.tabibu.plist"

if (( ! FOUND )); then
  echo "Nothing to remove -- no Tabibu installation artifacts found."
  exit 0
fi

if [[ "$MODE" == "delete" ]]; then
  echo "Tabibu has been uninstalled."
else
  echo "Dry run complete. Run '$0 --yes' to delete the items above."
fi
