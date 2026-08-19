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

Single self-contained executable — download it and rename it. Take it from the
**nightly** builds, not the stable releases:
<https://github.com/yt-dlp/yt-dlp-nightly-builds/releases>

Nightly rather than stable because a stable release is a nightly that was
blessed later: both are cut from master and pass the same CI, and the only
difference that reaches a user whose player has stopped working is how many
days old the fix is. The app moves its own copy onto the nightly channel on
first run regardless (`src/updater.rs`), so bundling stable would only mean
shipping a floor that is weeks behind and one extra channel switch to perform.

The bundled copy is a **floor**, not the binary that runs. On first launch the
app copies it into its app-data directory (`sidecar::seed`), and from then on
yt-dlp updates itself in place — daily, when the app is upgraded, and whenever
a playback failure suggests it has aged out. The bundle is what a machine with
no network falls back to.

Refresh it at each release, and smoke-test the one you are about to ship
against both providers before building the installer:

```bash
yt-dlp "ytsearch3:daft punk" --flat-playlist -J --no-warnings   # search
yt-dlp -f "bestaudio[ext=m4a]/bestaudio" -g <video-url>         # resolve
ffmpeg -i "<the url that printed>" -t 5 -vn -c copy probe.m4a   # play
```

The third line is the one that matters and the one that is easy to skip. A
resolve can succeed and still hand back a URL that YouTube refuses to anything
but the client it was minted for — which is exactly how this bundle went stale
in August 2026: search and resolve both worked, and every track 403'd the
moment ffmpeg reached for it.

## ffmpeg — must be a STATIC build

This is the easy mistake. A typical Windows ffmpeg install is a *shared* build:
`ffmpeg.exe` is only ~550 KB and depends on `avcodec-*.dll`, `avformat-*.dll`,
`avutil-*.dll` and friends sitting beside it. Tauri's `externalBin` bundles
single executables and will **not** carry those DLLs, so a shared build fails
at runtime with a missing-DLL error.

Use a static build, where `ffmpeg.exe` is one self-contained file (~90-170 MB).
From <https://github.com/BtbN/FFmpeg-Builds/releases>, the rule is the suffix:

| Asset                          | Use it? |
| ------------------------------ | ------- |
| `ffmpeg-…-win64-lgpl.zip`      | yes     |
| `ffmpeg-…-win64-gpl.zip`       | works, but see licensing below |
| `…-win64-lgpl-shared.zip`      | no      |
| `…-win64-gpl-shared.zip`       | no      |

**No `-shared` suffix means static.** That is the whole rule.

### Verifying, before you trust it

File size is not enough. Copy `ffmpeg.exe` *alone* into an empty directory and
run it — this reproduces exactly what Tauri does to it at install time, where it
lands beside the app executable and away from anything it shipped with:

```powershell
$iso = Join-Path $env:TEMP "ffmpeg-static-test"
New-Item -ItemType Directory -Force $iso | Out-Null
Copy-Item "<extracted>\bin\ffmpeg.exe" $iso
& "$iso\ffmpeg.exe" -version
```

A static build prints its version banner. A shared build fails immediately with
a missing-DLL error — the failure a user would otherwise hit on first play,
surfaced in five seconds instead of in a bug report.

Last verified with `ffmpeg-master-latest-win64-lgpl.zip` (N-126168, 110 MB).

### Licensing note

Most prebuilt static Windows binaries are GPL builds (`--enable-gpl`), which
would licence this app's whole distribution under the GPL. Prefer LGPL: nothing
here needs the GPL-only components, which are video *encoders*. This app only
decodes to raw PCM (`transcode.rs`), remuxes with `-c copy` for the offline
cache, and encodes PNG for cover art.

Confirm with the `configuration:` line that `ffmpeg -version` prints — an LGPL
build shows `--disable-libx264 --disable-libx265 --disable-libfdk-aac`. Check
also for `--enable-libopus` (Opus playback) and `--enable-schannel` (HTTPS,
without which no remote stream can be fetched at all).
