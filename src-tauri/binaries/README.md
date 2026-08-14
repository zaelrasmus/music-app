# Sidecar binaries

Tauri bundles these as `externalBin`. They are **not** in git: ffmpeg alone is
~100 MB, and they are platform-specific release artifacts rather than source.

Populate this directory before `tauri build`.

## Naming

Tauri requires the host target triple as a suffix, or it will not find the
binary. On this machine that is `x86_64-pc-windows-msvc`:

```
yt-dlp-x86_64-pc-windows-msvc.exe
ffmpeg-x86_64-pc-windows-msvc.exe
```

Check yours with `rustc -vV | grep host`.

## yt-dlp

Single self-contained executable — download the release build and rename it.
<https://github.com/yt-dlp/yt-dlp/releases>

The bundled copy is only a **floor**. yt-dlp breaks whenever YouTube changes,
so the app prefers a newer copy in its app-data directory when one is present
(see `src/sidecar.rs`); it never needs a recompile to pick up an update.

## ffmpeg — must be a STATIC build

This is the easy mistake. A typical Windows ffmpeg install is a *shared* build:
`ffmpeg.exe` is only ~550 KB and depends on `avcodec-*.dll`, `avformat-*.dll`,
`avutil-*.dll` and friends sitting beside it. Tauri's `externalBin` bundles
single executables and will **not** carry those DLLs, so a shared build fails
at runtime with a missing-DLL error.

Use a static build, where `ffmpeg.exe` is one self-contained file (~90-170 MB):

- <https://github.com/BtbN/FFmpeg-Builds/releases> — pick a `win64-*-shared`?
  **no** — pick the non-shared (static) archive.
- <https://www.gyan.dev/ffmpeg/builds/> — the "essentials" or "full" *release
  build* (not the shared variant).

Verify before bundling: the extracted `bin/ffmpeg.exe` should be tens of MB and
have no sibling `av*.dll` files.

### Licensing note

Most prebuilt static Windows binaries are GPL builds (`--enable-gpl`). That is
fine for personal use. If this app is ever distributed, prefer an LGPL build and
ship the corresponding notices — audio-only decoding does not need the GPL-only
components.
