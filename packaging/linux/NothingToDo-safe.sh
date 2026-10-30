#!/bin/sh
# Starts NothingToDo with WebKit's DMA-BUF renderer disabled, for GPUs or drivers that show a
# blank or flickering window.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if [ "$#" -gt 0 ] && [ -x "$1" ]; then
  application=$1
  shift
else
  application=""
  for candidate in "$script_dir"/NothingToDo*.AppImage "$script_dir"/nothingtodo; do
    if [ -f "$candidate" ] && [ -x "$candidate" ]; then
      application=$candidate
      break
    fi
  done
fi

if [ -z "$application" ]; then
  printf '%s\n' "NothingToDo safe mode could not find an adjacent AppImage or portable binary." >&2
  exit 1
fi

WEBKIT_DISABLE_DMABUF_RENDERER=1
export WEBKIT_DISABLE_DMABUF_RENDERER
exec "$application" "$@"
