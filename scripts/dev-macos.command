#!/bin/zsh
set -e

cd "$(dirname "$0")/../apps/desktop"
exec npm run tauri dev -- --config src-tauri/tauri.macos.json --config '{"identifier":"dev.ricohmonitor.debug","productName":"理光库存监控调试版","app":{"windows":[{"title":"理光库存监控调试版","width":900,"height":680,"minWidth":720,"minHeight":560}]}}'
