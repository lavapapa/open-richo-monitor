#!/bin/sh
set -eu
cd "$(dirname "$0")/../apps/desktop/src-tauri/icons"
# ImageMagick 内置 SVG 渲染器在此环境会丢失路径；显式绘制与 SVG 相同的几何。
body="M15 19H4a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v5"
detail="M5 4.5h2M7 7v10"
screen="M17 15h6v4h-6z"
stand="M20 19v2M18 21h4"
magick -size 44x44 canvas:none -fill none -stroke black -strokewidth 1.6 -draw "stroke-linecap round stroke-linejoin round scale 1.833333,1.833333 path '$body' path '$detail' circle 13,12 17,12 stroke-width 1.4 path '$screen' path '$stand'" PNG32:tray-macos.png
magick -size 32x32 canvas:none -stroke '#e9eff6' -strokewidth 1.6 -draw "stroke-linecap round stroke-linejoin round scale 1.333333,1.333333 fill '#152338' path '$body' fill none path '$detail' fill '#3183f1' circle 13,12 17,12 stroke-width 1.4 path '$screen' fill none path '$stand'" PNG32:tray-windows.png
