# SlideShowPro

Single-file Ken Burns slideshow viewer (`SlideShowPro.html`), plus an optional **thin Tauri Windows wrapper** for native file associations and "Open with".

## Web (browser)

Open `SlideShowPro.html` in a modern browser, or use the GitHub Pages deploy if configured.

## Desktop wrapper (Tauri 2, Windows sideload)

Thin shell only: loads the existing HTML, registers image extensions, and passes double-click / "Open with" paths into the viewer. Does **not** rewrite Ken Burns, playlist, virtualize, or Continue-from-Last.

### Prerequisites (Windows build machine)

- [Node.js](https://nodejs.org/) (npm or pnpm)
- [Rust](https://rustup.rs/) + MSVC toolchain (Visual Studio Build Tools)
- WebView2 (usually preinstalled on Windows 10/11)

### Build / run

```bash
npm install
npm run dev      # sync HTML into ui/ + tauri dev
npm run build    # sync HTML + NSIS installer under src-tauri/target/release/bundle/
```

`npm run sync-ui` copies `SlideShowPro.html` to `ui/index.html` (Tauri `frontendDist`).

### File associations

After installing the sideload NSIS package, these extensions open with SlideShowPro:

`jpg`, `jpeg`, `png`, `gif`, `webp`, `bmp`, `tif`, `tiff`, `ico`

Double-click or "Open with" passes paths on the CLI; Rust collects them and the HTML bridge loads them via a small invoke (`get_launch_paths` + `read_media_file`) into the existing `build()` path.

### Open Folder / native dialogs

Landing primary actions: **Open Files · Open Folder · Continue · Open Playlist** (Image Updates is under advanced).

In the Tauri shell, Open Files / Open Folder / Playlist use `@tauri-apps/plugin-dialog` (no visible `<input type=file>` chrome). Open Folder walks the chosen directory **recursively** with a sane image/video extension filter (cap 10 000). Browser builds use `showDirectoryPicker` when available.


### Out of scope

- Paid Apple signing / notarization / store listing
- Electron
- Rewriting the slideshow engine

Do not merge desktop PRs until Senior Dev GO.
