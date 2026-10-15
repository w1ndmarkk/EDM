<p align="center"><img src="docs/icon.png" width="96" alt="EDM"></p>

<h1 align="center">EDM</h1>

<p align="center">Multi-connection download manager for Windows 10/11 (64-bit), with a browser extension and video downloads.</p>

## Features

- Up to 16 connections per file, pause and resume
- IDM-style categories (Compressed, Documents, Music, Programs, Video) and file-type capture rules
- Extension for Chrome, Edge and Brave: takes over browser downloads and adds a "Download this video" panel (HLS, DASH, subtitles)
- YouTube videos and playlists via yt-dlp (yt-dlp, ffmpeg and deno are bundled)
- English and Turkish interface

## Install

Download the installer (`EDM_x.y.z_x64-setup.exe` or `.msi`) from [Releases](../../releases), then follow the extension setup in the app.

## Build

Requires Rust (stable) and the Tauri CLI: `cargo install tauri-cli --version "^2"`.

```powershell
./scripts/fetch-tools.ps1   # downloads yt-dlp, ffmpeg and deno into Tools/
./scripts/build.ps1         # runs the tests and puts the installers in build/
```

## Türkçe

Windows 10/11 için çok bağlantılı indirme yöneticisi. Tarayıcı indirmelerini devralır, sayfalardaki videoları ve YouTube oynatma listelerini indirir. Kurulum dosyası [Releases](../../releases) sayfasında; arayüz Türkçe.

## License

[MIT](LICENSE). Based on [MC Download Manager](https://github.com/mstcvk/mc-download-manager) by Mesut Çevik.
