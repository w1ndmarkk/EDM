# Release build: bundled tools + Tauri build → build\ (NSIS setup.exe and .msi).
#
#   powershell -ExecutionPolicy Bypass -File scripts\build.ps1
#
# Requirements: Rust (rustup, stable MSVC toolchain), Visual Studio Build Tools with the C++ workload,
# and the Tauri CLI (installed on first run with `cargo install tauri-cli --version "^2" --locked`).
# Code signing: set TAURI_SIGNING_* / configure bundle.windows.certificateThumbprint in src-tauri\tauri.conf.json.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

Write-Host '> Tools (yt-dlp, ffmpeg, deno)'
$missing = 'yt-dlp.exe', 'ffmpeg.exe', 'deno.exe' | Where-Object { -not (Test-Path "$root\Tools\$_") }
if ($missing) {
    & "$PSScriptRoot\fetch-tools.ps1"
}

Write-Host '> Tauri CLI'
cargo tauri --version 2>$null | Out-Null
if ($LASTEXITCODE -ne 0) { cargo install tauri-cli --version '^2' --locked }

Write-Host '> Unit tests'
Push-Location "$root\src-tauri"
try {
    cargo test --quiet
    if ($LASTEXITCODE -ne 0) { throw 'tests failed' }

    Write-Host '> Release build (NSIS + MSI)'
    # Drop installers of earlier builds so only this version is collected below.
    Remove-Item "$root\src-tauri\target\release\bundle\nsis\*.exe", "$root\src-tauri\target\release\bundle\msi\*.msi" -ErrorAction SilentlyContinue
    cargo tauri build
    if ($LASTEXITCODE -ne 0) { throw 'tauri build failed' }
} finally {
    Pop-Location
}

$version = (Get-Content "$root\src-tauri\tauri.conf.json" -Raw | ConvertFrom-Json).version
$out = Join-Path $root 'build'
New-Item -ItemType Directory -Force $out | Out-Null
$bundle = "$root\src-tauri\target\release\bundle"
Get-ChildItem "$bundle\nsis\*.exe", "$bundle\msi\*.msi" | ForEach-Object {
    Copy-Item $_.FullName $out -Force
    Write-Host ("  {0}  ({1:N1} MB)" -f $_.Name, ($_.Length / 1MB))
}
Write-Host "Ready: $out (version $version)"
