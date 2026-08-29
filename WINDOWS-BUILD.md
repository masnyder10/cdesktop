# Building cdesktop for Windows

This fork adds a reproducible Windows desktop build. Upstream
(`cdesktop-ai/cdesktop`) has Tauri scaffolding but only ships macOS/Linux
artifacts; its `local-build.sh` is POSIX-only and its MSI path
(`scripts/build-tauri-msi.js`) targets Linux `wixl` from msitools for
cross-compilation. The steps below build natively on Windows with Tauri's
own WiX/NSIS bundlers, so msitools is **not** required.

## Prerequisites

| Component | Version used | Notes |
|---|---|---|
| Windows | 11 Pro, build 26200 | x64 |
| Rust | `nightly-2025-12-04` | Pinned by `rust-toolchain.toml`; rustup installs it automatically. Do **not** force stable — the workspace is `edition = "2024"`. |
| MSVC | VS **2019** Build Tools, "Desktop development with C++" | 2022 is *not* required. See "VS detection" below. |
| Windows SDK | 10.0.19041.0 | Must have both `Include` and `Lib` under `C:\Program Files (x86)\Windows Kits\10`. |
| Node.js | 22.19.0 | `>= 20` per `package.json` engines |
| pnpm | 10.13.1 | `>= 8` |
| WebView2 | ships with Windows 11 | Tauri uses the system WebView2 — no bundled Chromium. |
| LLVM / libclang | any recent LLVM | **Required.** See "libclang is mandatory" below. |

### libclang is mandatory

The workspace **cannot** be built on Windows without `libclang.dll`. This is not
optional and not a warning you can skip — the build fails hard with:

```
error: failed to run custom build command for `libsqlite3-sys v0.30.1`
Unable to find libclang: couldn't find any valid shared libraries matching:
['clang.dll', 'libclang.dll']
```

The requirement is forced by a real application feature:

```
crates/db, crates/server, crates/services
  -> sqlx feature "sqlite-preupdate-hook"
     -> sqlx-sqlite/preupdate-hook
        -> libsqlite3-sys/preupdate_hook
           -> libsqlite3-sys/buildtime_bindgen   (forces bindgen)
              -> bindgen requires libclang
```

Do **not** try to fix this by dropping `sqlite-preupdate-hook`.
`crates/services/src/services/events.rs` calls `handle.set_preupdate_hook(...)`
to emit live row-deletion patches into the message store, which is what drives
the live-updating UI. Removing the feature fails to compile and breaks that
behaviour.

Upstream never hits this because its CI cross-compiles Windows targets from
Linux (`cargo xwin --cross-compiler clang-cl`), where clang is already present.

Install LLVM:

```powershell
winget install --id LLVM.LLVM --exact
```

If the build still cannot find it, point at it explicitly:

```powershell
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
```

Install the Tauri CLI (it is not vendored in the repo):

```powershell
cargo install tauri-cli --version "^2" --locked
```

If `cargo` is not on PATH, it lives at `%USERPROFILE%\.cargo\bin`.

### VS detection false negative

`cargo tauri info` reports:

```
[X] Couldn't detect any Visual Studio or VS Build Tools instance with MSVC and SDK components.
```

On this machine that is a **false negative** and can be ignored. Tauri probes
`vswhere -requires Microsoft.VisualStudio.Component.Windows10SDK.19041`. VS
Build Tools 2019 *does* provide the MSVC toolset
(`Microsoft.VisualStudio.Component.VC.Tools.x86.x64` resolves), and SDK
10.0.19041.0 *is* installed, but it was registered outside that VS component
ID, so the probe misses it. Linking works. Verify for yourself with:

```powershell
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
& $vswhere -all -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property displayName
Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\Lib" -Directory | Select-Object Name
```

Only install Build Tools 2022 if the **link step** actually fails.

## Build

```powershell
pnpm i
cd crates\tauri-app
cargo tauri build
```

`beforeBuildCommand` in `tauri.conf.json` runs
`pnpm --filter @vibe/local-web run build` first. **This is required, not
optional:** `crates/server` embeds the frontend at compile time via

```rust
#[derive(RustEmbed)]
#[folder = "../../packages/local-web/dist"]
```

If `packages/local-web/dist` does not exist, the Rust build still succeeds and
you get an app that opens a **blank window**. That is the single most common
failure mode here.

### Memory

The link stage is the peak. This machine has 16 GB and the workspace is large.
If the linker OOMs, cap parallelism:

```powershell
$env:CARGO_BUILD_JOBS=4   # or 2 if still tight
```

Before building, close other VS Code windows and confirm Docker/WSL are down:

```powershell
Get-Process *docker* -ErrorAction SilentlyContinue
wsl --list --running
```

Expect 20-40 minutes cold.

## Artifacts

A successful build prints `Finished 2 bundles at` and produces:

```
target\release\bundle\msi\cdesktop-mr_0.2.3_x64_en-US.msi     (52 MB, WiX)
target\release\bundle\nsis\cdesktop-mr_0.2.3_x64-setup.exe    (38 MB, NSIS)
target\release\cdesktop-tauri.exe                             (160 MB, raw binary)
```

Tauri downloads WiX 3.14 and NSIS 3.11 automatically on first bundle. Neither
needs to be installed beforehand, and neither requires Visual Studio 2022.

Silent install:

```powershell
.\cdesktop-mr_0.2.3_x64-setup.exe /S
```

The NSIS package installs per-user to `%LOCALAPPDATA%\cdesktop-mr` with no
elevation, and creates both a Start menu entry and a desktop shortcut.

Build time on an i9-10900K with `CARGO_BUILD_JOBS=4` was 15m24s for the Rust
compile, plus about a minute for the frontend.

## Notes and gotchas

- **Updater is disabled in this fork.** Upstream ships
  `plugins.updater.endpoints: ["__TAURI_UPDATE_ENDPOINT__"]`, which is not a
  valid URL and fails config validation; CI substitutes the real endpoint and
  `local-build.sh` rewrites it to a dummy. This fork sets it to
  `https://localhost/disabled` and sets `bundle.createUpdaterArtifacts: false`,
  because signing updater artifacts requires upstream's private key
  (`TAURI_SIGNING_PRIVATE_KEY`), which a fork does not have.
- **Do not run `local-build.sh` on Windows.** It is bash-only and assumes
  `zip`, `cp` to extensionless binary names, and macOS/Linux bundle layouts.
- **`scripts/build-tauri-msi.js` is not used here.** It shells out to `wixl`
  (Linux msitools) and is upstream's cross-compile path.
- **Sentry telemetry is pre-existing upstream.** The Vite build runs
  `sentry-vite-plugin`, which emits build telemetry and attempts a source-map
  upload. Without `SENTRY_AUTH_TOKEN` the upload fails harmlessly and nothing
  is uploaded. This fork neither added nor removed it.
- **Line endings.** Git may report `crates/tauri-app/Cargo.toml` and
  `packages/local-web/src/routeTree.gen.ts` as modified with an empty diff.
  That is LF -> CRLF normalization from Windows tooling, not a content change.
  `routeTree.gen.ts` is regenerated by the TanStack Router Vite plugin.
