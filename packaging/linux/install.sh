#!/bin/sh
# Registers this portable NothingToDo folder with the desktop, where it sits:
#
#   launcher   ~/.local/share/applications/uk.co.astraeaoss.nothingtodo.desktop
#   icons      ~/.local/share/icons/hicolor/<size>/apps/uk.co.astraeaoss.nothingtodo.png
#   autostart  ~/.config/systemd/user/nothingtodo.service (only with --autostart)
#
# Nothing is copied: the app and its "NothingToDo Data" folder stay here. After moving the
# folder, run this again from the new place.
#
#   ./install.sh               add the launcher and icons
#   ./install.sh --autostart   also start NothingToDo in the tray at login
#   ./install.sh --uninstall   remove all of the above; the data folder is left alone
set -eu

app_id=uk.co.astraeaoss.nothingtodo
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
binary="$here/nothingtodo"
data_home=${XDG_DATA_HOME:-$HOME/.local/share}
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
applications="$data_home/applications"
icons="$data_home/icons/hicolor"
unit_dir="$config_home/systemd/user"
unit=nothingtodo.service

autostart=0
uninstall=0
for argument in "$@"; do
  case $argument in
    --autostart) autostart=1 ;;
    --uninstall) uninstall=1 ;;
    *) echo "usage: $0 [--autostart | --uninstall]" >&2; exit 2 ;;
  esac
done

refresh() {
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q "$applications" || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "$icons" || true
  fi
}

if [ "$uninstall" = 1 ]; then
  if [ -f "$unit_dir/$unit" ]; then
    systemctl --user disable "$unit" >/dev/null 2>&1 || true
    rm -f "$unit_dir/$unit"
    systemctl --user daemon-reload || true
  fi
  rm -f "$applications/$app_id.desktop"
  for icon in "$icons"/*/apps/"$app_id.png"; do
    rm -f "$icon"
  done
  refresh
  echo "Removed the NothingToDo launcher, icons and autostart. Tasks in $here/NothingToDo Data are untouched."
  exit 0
fi

if [ ! -x "$binary" ]; then
  echo "No nothingtodo binary beside this script; run it from the extracted portable folder." >&2
  exit 1
fi

# Desktop entries and systemd units each have their own quoting rules; refuse the characters
# that need escaping in either rather than risk a launcher that runs the wrong thing.
case $here in
  *[\"\\\`\$%]*|*'
'*)
    echo "This folder's path contains a quote, backslash, \$, % or newline. Move it somewhere plainer and run again." >&2
    exit 1
    ;;
esac

mkdir -p "$applications"
sed "s|^Exec=.*|Exec=\"$binary\"|; s|^TryExec=.*|TryExec=$binary|" \
  "$here/$app_id.desktop" > "$applications/.$app_id.desktop.new"
mv -f "$applications/.$app_id.desktop.new" "$applications/$app_id.desktop"

for size_dir in "$here"/share/icons/hicolor/*; do
  size=$(basename "$size_dir")
  mkdir -p "$icons/$size/apps"
  cp "$size_dir/apps/$app_id.png" "$icons/$size/apps/$app_id.png"
done
refresh
echo "Added NothingToDo to the app launcher, running from $here"

# A re-run after moving the folder keeps autostart on and points it at the new place.
if [ -f "$unit_dir/$unit" ]; then
  autostart=1
fi

if [ "$autostart" = 1 ]; then
  mkdir -p "$unit_dir"
  cat > "$unit_dir/$unit" <<UNIT
# Installed by $here/install.sh; remove with ./install.sh --uninstall.
[Unit]
Description=NothingToDo (tray, reminders)
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart="$binary" --hidden
Restart=on-failure
RestartSec=5

[Install]
WantedBy=graphical-session.target
UNIT
  systemctl --user daemon-reload
  systemctl --user enable "$unit" >/dev/null
  echo "NothingToDo will start in the tray at your next login."
fi
