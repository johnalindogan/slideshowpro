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

`npm run sync-ui` copies `SlideShowPro.html` – `ui/index.html` (Tauri `frontendDist`).

### File associations

After installing the sideload NSIS package, these extensions open with SlideShowPro:

`jpg`, `jpeg`, `png`, `gif`, `webp`, `bmp`, `tif`, `tiff`, `ico`

Double-click or "Open with" passes paths on the CLI; Rust collects them and the HTML bridge loads them via a small invoke (`get_launch_paths` + `read_media_file`) into the existing `build()` path.

### Out of scope

- Paid Apple signing / notarization / store listing
- Electron
- Rewriting the slideshow engine

Do not merge desktop PRs until Senior Dev GO.
