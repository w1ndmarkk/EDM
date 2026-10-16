<p align="center"><img src="docs/icon.png" width="96" alt="EDM"></p>

<h1 align="center">EDM</h1>

<p align="center">Multi-connection download manager for Windows 10/11 (64-bit), with a browser extension and video downloads.</p>

## Features

- Up to 16 connections per file, pause and resume
- IDM-style categories (Compressed, Documents, Music, Programs, Video) and file-type capture rules
- Extension for Chrome, Edge, Brave and Firefox: takes over browser downloads and adds a "Download this video" panel (HLS, DASH, audio, subtitles)
- YouTube videos and playlists via yt-dlp (yt-dlp, ffmpeg and deno are bundled)
- English and Turkish interface

## Install

1. Download the installer (`EDM_x.y.z_x64-setup.exe` or `.msi`) from [Releases](../../releases) and run it.
2. On first start EDM offers a guided extension setup for each installed browser (later: *Settings ▸ Browser Extensions*):
   - **Firefox:** the add-on is signed by Mozilla; *Install in Firefox* opens it and Firefox asks to add it.
     It is also attached to each release as `edm-firefox.xpi`.
   - **Chrome, Edge, Brave:** the browser stores do not offer EDM, so the extension is loaded from a folder
     (*Developer mode* ▸ *Load unpacked*); the setup prepares the folder and walks you through it.

## Build

Requires Rust (stable) and the Tauri CLI: `cargo install tauri-cli --version "^2"`.

```powershell
./scripts/fetch-tools.ps1   # downloads yt-dlp, ffmpeg and deno into Tools/
./scripts/build.ps1         # runs the tests and puts the installers in build/
```

The extension packages are made by `./scripts/pack-extension.ps1`. When the extension changes, sign
`build/edm-extension-firefox-<version>.zip` on [addons.mozilla.org](https://addons.mozilla.org/developers/)
and replace `Extension-signed/edm-firefox.xpi` (`build.ps1` warns while they differ).

## Türkçe

Windows 10/11 için çok bağlantılı indirme yöneticisi. Tarayıcı indirmelerini devralır, sayfalardaki videoları ve YouTube oynatma listelerini indirir; arayüz Türkçe.

Kurulum dosyasını [Releases](../../releases) sayfasından indirip çalıştırın. İlk açılışta EDM, kurulu her tarayıcı için eklenti kurulumunda yol gösterir: Firefox'ta imzalı eklenti tek tıkla kurulur, Chrome, Edge ve Brave'de eklenti Geliştirici modunda bir klasörden yüklenir.

## License

[MIT](LICENSE). Based on [MC Download Manager](https://github.com/mstcvk/mc-download-manager) by Mesut Çevik.
