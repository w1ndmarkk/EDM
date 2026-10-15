# Packs the browser extension for the Chrome Web Store / Edge Add-ons → build\edm-extension-<version>.zip
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$version = (Get-Content "$root\Extension\manifest.json" -Raw | ConvertFrom-Json).version
$out = Join-Path $root 'build'
New-Item -ItemType Directory -Force $out | Out-Null
$zip = Join-Path $out "edm-extension-$version.zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path "$root\Extension\*" -DestinationPath $zip
Write-Host "Ready: $zip"
