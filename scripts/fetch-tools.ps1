# Downloads the yt-dlp, ffmpeg and deno binaries that get bundled into the installer (into Tools\).
# Sources (all verified against the published SHA-256):
#   yt-dlp : https://github.com/yt-dlp/yt-dlp/releases          (yt-dlp.exe)
#   ffmpeg : https://www.gyan.dev/ffmpeg/builds                 (release "essentials", static win64, GPL)
#   deno   : https://github.com/denoland/deno/releases          (deno-x86_64-pc-windows-msvc.zip, MIT)
# Override with $env:YTDLP_URL / $env:FFMPEG_ZIP_URL (and *_SHA256_URL) to use other builds,
# e.g. an LGPL ffmpeg if you distribute builds and want to avoid the GPL obligations.
param([switch]$Force)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$tools = Join-Path $root 'Tools'
New-Item -ItemType Directory -Force $tools | Out-Null

$ytdlpUrl = if ($env:YTDLP_URL) { $env:YTDLP_URL } else { 'https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe' }
$ytdlpSums = if ($env:YTDLP_SHA256_URL) { $env:YTDLP_SHA256_URL } else { 'https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS' }
$ffmpegUrl = if ($env:FFMPEG_ZIP_URL) { $env:FFMPEG_ZIP_URL } else { 'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip' }
$ffmpegSum = if ($env:FFMPEG_SHA256_URL) { $env:FFMPEG_SHA256_URL } else { "$ffmpegUrl.sha256" }
# deno: the JavaScript runtime yt-dlp needs for YouTube (https://github.com/yt-dlp/yt-dlp/wiki/EJS).
$denoUrl = if ($env:DENO_ZIP_URL) { $env:DENO_ZIP_URL } else { 'https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip' }

function Get-File([string]$url, [string]$out) {
    # curl.exe ships with Windows 10 1803+ and is much faster than Invoke-WebRequest for large files.
    & curl.exe -fsSL --retry 3 -o $out $url
    if ($LASTEXITCODE -ne 0) { throw "download failed: $url" }
}

function Get-ExpectedHash([string]$url, [string]$name) {
    $text = (& curl.exe -fsSL --retry 3 $url) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw "checksum download failed: $url" }
    foreach ($line in $text -split "`n") {
        $parts = $line.Trim() -split '\s+'
        if ($parts[0] -match '^[0-9a-fA-F]{64}$' -and ($parts.Count -eq 1 -or -not $name -or $parts[-1] -eq $name)) {
            return $parts[0].ToLower()
        }
    }
    throw "no checksum for $name in $url"
}

function Assert-Hash([string]$file, [string]$expected) {
    $actual = (Get-FileHash -Algorithm SHA256 $file).Hash.ToLower()
    if ($actual -ne $expected) { Remove-Item -Force $file; throw "checksum mismatch for $file" }
}

$ytdlp = Join-Path $tools 'yt-dlp.exe'
if ($Force -or -not (Test-Path $ytdlp)) {
    Write-Host '-> downloading yt-dlp...'
    Get-File $ytdlpUrl $ytdlp
    Assert-Hash $ytdlp (Get-ExpectedHash $ytdlpSums 'yt-dlp.exe')
}

$ffmpeg = Join-Path $tools 'ffmpeg.exe'
if ($Force -or -not (Test-Path $ffmpeg)) {
    Write-Host '-> downloading ffmpeg...'
    $zip = Join-Path $tools 'ffmpeg.zip'
    $unpack = Join-Path $tools '_ffmpeg'
    Get-File $ffmpegUrl $zip
    Assert-Hash $zip (Get-ExpectedHash $ffmpegSum '')
    if (Test-Path $unpack) { Remove-Item -Recurse -Force $unpack }
    Expand-Archive -Path $zip -DestinationPath $unpack
    $exe = Get-ChildItem -Recurse -Path $unpack -Filter ffmpeg.exe | Select-Object -First 1
    if (-not $exe) { throw 'ffmpeg.exe not found in the archive' }
    Copy-Item $exe.FullName $ffmpeg -Force
    Remove-Item -Recurse -Force $unpack, $zip
}

$deno = Join-Path $tools 'deno.exe'
if ($Force -or -not (Test-Path $deno)) {
    Write-Host '-> downloading deno...'
    $zip = Join-Path $tools 'deno.zip'
    $unpack = Join-Path $tools '_deno'
    Get-File $denoUrl $zip
    # The checksum file is either "<hash>  <name>" or a Get-FileHash table: take the 64-hex token.
    $sumText = (& curl.exe -fsSL --retry 3 "$denoUrl.sha256sum") -join ' '
    $expected = ($sumText -split '\s+' | Where-Object { $_ -match '^[0-9a-fA-F]{64}$' } | Select-Object -First 1)
    if (-not $expected) { throw 'no checksum for deno' }
    Assert-Hash $zip $expected.ToLower()
    if (Test-Path $unpack) { Remove-Item -Recurse -Force $unpack }
    Expand-Archive -Path $zip -DestinationPath $unpack
    Copy-Item (Join-Path $unpack 'deno.exe') $deno -Force
    Remove-Item -Recurse -Force $unpack, $zip
}

# Files downloaded by curl carry no Mark of the Web, but clear it in case they were copied in by hand.
Get-ChildItem $tools -Filter *.exe | Unblock-File
Write-Host 'Tools ready:'
& $ytdlp --version
& $ffmpeg -hide_banner -version | Select-Object -First 1
& $deno --version | Select-Object -First 1
