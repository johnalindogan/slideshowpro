# SlideShowX Features

SlideShowX is a single-file slideshow viewer and playlist tool. The current app includes:

## Media Import

- Open image and video files from the file picker.
- Drag and drop files onto the landing screen.
- Add more files later without restarting the session.
- Load playlists from JSON.

## Playback

- Play and pause slides.
- Move to the next or previous slide.
- Reverse playback.
- Adjust playback speed (toolbar buttons or keyboard: `↑` / `↓` ±5% by default; `,` / `.` remain aliases; remappable via the key editor / `ssp_keymap`). Toast shows the new rate when chrome is hidden.
- While a **video** is current, `←` / `→` (prev/next bindings) seek **−2s / +2s** (clamped). **Double** the same key within **350ms** jumps to the previous/next playlist item immediately (a press after 350ms is a fresh seek). At the start, `←` goes to the previous media; at the end, `→` goes to the next. Images keep single-press slide prev/next.
- Set a default duration for photos.
- Auto-advance through media during playback.

## Viewing Modes

- Fullscreen viewing.
- Watch mode with a cleaner presentation view.
- Hide and show controls while viewing.
- Toggle filename display.
- Show slide progress and current position.

## Image and Video Controls

- Zoom in and out.
- Reset pan and zoom.
- Videos show the **full frame** (`object-fit: contain`, letterbox/pillarbox OK). Ken Burns motion stays **images-only**; zoom/pan/drag still apply to the uncropped video frame.
- Mirror horizontally or vertically.
- Rotate clockwise or counterclockwise.
- Mute playback and adjust volume.

## Color Controls

- Adjust brightness.
- Adjust contrast.
- Adjust saturation.
- Adjust hue.
- Adjust highlights.
- Adjust lowlights (shadows).
- Adjust gamma (mids).
- Adjust sharpness (SVG convolve / baked convolution on export).
- Nudge lowlights / highlights / gamma / sharpness from the keyboard (defaults: Y/U, I/O, 5/6, [/]).
- Reset any color adjustment back to default.
- Grades persist per item in `imageUpdates.clr` (localStorage + playlist save).

## Layout Modes

- Horizontal Max mode for portrait media on landscape screens.
- Vertical Max mode for landscape media on portrait screens.
- 3-panel max modes for same-side layouts.

## Playlist

- Undock the Media Manager into its own native desktop window (Windows Tauri), including its toolbar (add/folder/shuffle/save/load). Dock back or close the window to restore chrome on the main window without losing playlist state.

## Playlist Management

- Open and manage playlists in a side panel.
- Shuffle the playlist.
- Create folders inside the playlist.
- Reorder items by drag and drop.
- Duplicate items in the playlist.
- Remove items from the playlist.
- Delete files from the system with playlist cleanup.
- Save and reload playlists.

## Ken Burns

- Enable or disable Ken Burns motion.
- Control zoom intensity.
- Control pan intensity.
- Choose motion direction.
- Choose easing behavior.
- Set per-slide Ken Burns presets for individual images.

## Keyboard Shortcuts

- Customizable shortcut assignment editor (toolbar keyboard button) lists remappable commands from the app key map.
- Click a key badge and press a new key to assign; remaps persist in `localStorage` (`ssp_keymap`).
- Conflict detection: two commands cannot silently share a key (prior owner is unbound, with a toast).
- Per-command reset and **Reset all to defaults**.
- Defaults include navigation (`←`/`→`; context-sensitive seek on video), play/pause, speed ± (`↑`/`↓`, with `,`/`.` aliases), zoom, fullscreen, mute, mirror, rotation, hide controls, filename toggle, edit nudges, screenshot, and export graded (`E`).
- Fixed (not remappable): Delete / Backspace, Alt+Arrows pan (does not collide with speed arrows), Escape.

## Persistence

- Save per-item settings in local storage.
- Restore previous session data when available.
- Persist playlist and slide-specific adjustments.
- Store image adjustment metadata separately from file list entries.

## Export and Capture

- Save a screenshot of the current view with grades baked (including lowlights/highlights/gamma/sharpness), auto-saving to a Screenshots folder when supported.
- **Export Graded** (toolbar or `E`): for images, write a JPEG with the full color suite baked via the native save dialog (Tauri) or browser download. For video, best-effort canvas/MediaRecorder WebM bake for short clips (≤45s); otherwise export a `.sspgrade.json` sidecar documenting the grade (ffmpeg is not bundled). Playlist save still keeps grades on the item.

## Supported Media

- Images.
- MP4 video.
- MOV video.
- WebM video.

