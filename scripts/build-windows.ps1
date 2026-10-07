$ErrorActionPreference = 'Stop'

if ($env:OS -ne 'Windows_NT') {
    throw '请在 Windows 开发环境中运行此脚本。'
}

$projectRoot = Split-Path $PSScriptRoot -Parent
$env:CARGO_TARGET_DIR = Join-Path $projectRoot 'target'
$target = 'x86_64-pc-windows-msvc'

function Invoke-Checked([scriptblock] $Action) {
    & $Action
    if ($LASTEXITCODE -ne 0) {
        throw "上一步测试或构建失败，退出码为 $LASTEXITCODE。"
    }
}

Push-Location (Join-Path $projectRoot 'apps/notification-runtime')
try {
    Invoke-Checked { npm.cmd ci }
    Invoke-Checked { npm.cmd test }
} finally {
    Pop-Location
}

Push-Location (Join-Path $projectRoot 'apps/desktop')
try {
    Invoke-Checked { npm.cmd ci }
    Invoke-Checked { npm.cmd run check }
    Invoke-Checked { npm.cmd test -- --maxWorkers=2 }
    Invoke-Checked { npm.cmd run build }
    Invoke-Checked { cargo.exe test --locked --manifest-path ../../crates/core/Cargo.toml --target $target -- --test-threads=2 }
    Invoke-Checked { cargo.exe test --locked --manifest-path src-tauri/Cargo.toml --target $target }
    Invoke-Checked { npm.cmd run tauri -- build --target $target --bundles nsis --config src-tauri/tauri.windows-x64.json }
} finally {
    Pop-Location
}

$bundleDirectory = Join-Path $env:CARGO_TARGET_DIR "$target/release/bundle/nsis"
$installer = Get-ChildItem $bundleDirectory -Filter '*-setup.exe' |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
$outputDirectory = Join-Path $projectRoot 'dist/windows-x86_64'
New-Item -ItemType Directory -Force $outputDirectory | Out-Null
Copy-Item $installer.FullName $outputDirectory -Force
Copy-Item ($installer.FullName + '.sig') $outputDirectory -Force
Write-Host "安装程序：$(Join-Path $outputDirectory $installer.Name)"
