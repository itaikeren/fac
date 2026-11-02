#!/bin/sh
# screenshot.sh <window-id> <out.png> [crop x y w h]
#
# Captures one window by its CGWindowID (no shadow, no sound), never the whole
# screen. With "crop x y w h" it keeps only that rectangle, in image pixels
# (Retina captures are 2x the window's points). Commit only cropped images.
set -eu

id=${1:?usage: screenshot.sh <window-id> <out.png> [crop x y w h]}
out=${2:?usage: screenshot.sh <window-id> <out.png> [crop x y w h]}

screencapture -o -x -l "$id" "$out"

if [ "${3:-}" = crop ]; then
  x=${4:?crop needs x y w h}; y=${5:?}; w=${6:?}; h=${7:?}
  sips --cropOffset "$y" "$x" --cropToHeightWidth "$h" "$w" "$out" --out "$out" >/dev/null
fi
sips -g pixelWidth -g pixelHeight "$out" | tail -n 2
