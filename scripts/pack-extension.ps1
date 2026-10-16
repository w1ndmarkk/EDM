# Packs the browser extension:
#   build\edm-extension-<version>.zip          Chrome Web Store / Edge Add-ons
#   build\edm-extension-firefox-<version>.zip  addons.mozilla.org (Firefox manifest, see below)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$root = Split-Path -Parent $PSScriptRoot
$ext = Join-Path $root 'Extension'
$manifest = Get-Content "$ext\manifest.json" -Raw -Encoding UTF8
$version = ($manifest | ConvertFrom-Json).version
$out = Join-Path $root 'build'
New-Item -ItemType Directory -Force $out | Out-Null

# Same changes as browsers::firefox_flavor in the app (keep the two in step): blocking webRequest, a background script instead of a
# service worker, the add-on ID, no Chrome-only keys.
function Get-FirefoxManifest([string]$json) {
    $m = $json | ConvertFrom-Json
    $m.background = [pscustomobject]@{ scripts = @('hls.js', 'rules.js', 'background.js') }
    if ($m.permissions -notcontains 'webRequestBlocking') { $m.permissions = @($m.permissions) + 'webRequestBlocking' }
    $gecko = [pscustomobject]@{ id = 'edm@w1ndmarkk'; strict_min_version = '128.0'; data_collection_permissions = [pscustomobject]@{ required = @('none') } }
    $m | Add-Member -NotePropertyName browser_specific_settings -NotePropertyValue ([pscustomobject]@{ gecko = $gecko }) -Force
    $m.PSObject.Properties.Remove('minimum_chrome_version')
    foreach ($cs in $m.content_scripts) {
        $cs.PSObject.Properties.Remove('match_origin_as_fallback')
        $cs | Add-Member -NotePropertyName match_about_blank -NotePropertyValue $true -Force
    }
    $m | ConvertTo-Json -Depth 10
}

# Zip entries with forward slashes (Compress-Archive in Windows PowerShell writes backslashes, which stores reject).
function New-ExtensionZip([string]$zip, [string]$manifestText) {
    if (Test-Path $zip) { Remove-Item $zip }
    $archive = [IO.Compression.ZipFile]::Open($zip, 'Create')
    try {
        foreach ($file in Get-ChildItem $ext -Recurse -File) {
            $name = $file.FullName.Substring($ext.Length + 1).Replace('\', '/')
            $entry = $archive.CreateEntry($name, 'Optimal')
            $stream = $entry.Open()
            try {
                $bytes = if ($name -eq 'manifest.json') { (New-Object Text.UTF8Encoding $false).GetBytes($manifestText) } else { [IO.File]::ReadAllBytes($file.FullName) }
                $stream.Write($bytes, 0, $bytes.Length)
            } finally { $stream.Dispose() }
        }
    } finally { $archive.Dispose() }
    Write-Host "Ready: $zip"
}

New-ExtensionZip (Join-Path $out "edm-extension-$version.zip") $manifest
New-ExtensionZip (Join-Path $out "edm-extension-firefox-$version.zip") (Get-FirefoxManifest $manifest)
