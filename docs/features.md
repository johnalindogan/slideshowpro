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
- Adjust playback speed (toolbar buttons or keyboard: `,` slower / `.` faster by default; remappable via the key editor / `ssp_keymap`).
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
- Mirror horizontally or vertically.
- Rotate clockwise or counterclockwise.
- Mute playback and adjust volume.

## Color Controls

- Adjust brightness.
- Adjust contrast.
- Adjust saturation.
- Adjust hue.
- Adjust highlights.
- Adjust shadows.
- Adjust gamma.
- Reset any color adjustment back to default.

## Layout Modes

- Horizontal Max mode for portrait media on landscape screens.
- Vertical Max mode for landscape media on portrait screens.
- 3-panel max modes for same-side layouts.

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
- Defaults include navigation, play/pause, speed ± (`,` / `.`), zoom, fullscreen, mute, mirror, rotation, hide controls, filename toggle, and edit nudges.
- Fixed (not remappable): Delete / Backspace, Alt+Arrows pan, Escape.

## Persistence

- Save per-item settings in local storage.
- Restore previous session data when available.
- Persist playlist and slide-specific adjustments.
- Store image adjustment metadata separately from file list entries.

## Export and Capture

- Save a screenshot of the current view, auto-saving to a Screenshots folder when supported.

## Supported Media

- Images.
- MP4 video.
- MOV video.
- WebM video.

