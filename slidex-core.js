/* SlideX pure helpers: crop, export, playlists, locate-folder, framing.
   Loaded as a classic script (window.SlideXCore). Unit tests eval this file. */
(function (root) {
  'use strict';

  var CROP_VERSION = 1;
  var PLAYLIST_SSP_VERSION = 1;
  var ASPECTS = { free: null, '16:9': 16 / 9, '4:3': 4 / 3, '1:1': 1, '9:16': 9 / 16 };
  var CLR_DEF = { b: 100, c: 100, s: 100, h: 0, hl: 0, sh: 0, g: 100, shp: 100 };

  function orientedSize(rawW, rawH, orientation) {
    var o = orientation | 0;
    if (o >= 5 && o <= 8) return { w: rawH, h: rawW };
    return { w: rawW, h: rawH };
  }

  function clamp01Crop(crop) {
    var w = Math.max(0.02, Math.min(1, crop.w));
    var h = Math.max(0.02, Math.min(1, crop.h));
    var x = Math.max(0, Math.min(1 - w, crop.x));
    var y = Math.max(0, Math.min(1 - h, crop.y));
    var aspect = Object.prototype.hasOwnProperty.call(ASPECTS, crop.aspect) ? crop.aspect : 'free';
    return { x: x, y: y, w: w, h: h, aspect: aspect };
  }

  function normalizeCrop(crop) {
    if (!crop || typeof crop !== 'object') return null;
    var x = Number(crop.x), y = Number(crop.y), w = Number(crop.w), h = Number(crop.h);
    if (![x, y, w, h].every(function (n) { return Number.isFinite(n); })) return null;
    if (w <= 0 || h <= 0) return null;
    return clamp01Crop({ x: x, y: y, w: w, h: h, aspect: crop.aspect || 'free' });
  }

  function isActiveCrop(crop) {
    var c = normalizeCrop(crop);
    if (!c) return false;
    return c.x > 0.001 || c.y > 0.001 || c.w < 0.999 || c.h < 0.999;
  }

  function applyAspect(crop, aspectKey, dispW, dispH) {
    var ratio = ASPECTS[aspectKey];
    var base = crop && Number.isFinite(Number(crop.x)) ? crop : { x: 0, y: 0, w: 1, h: 1 };
    if (!ratio || !(dispW > 0) || !(dispH > 0)) {
      var plain = normalizeCrop(base) || { x: 0, y: 0, w: 1, h: 1, aspect: 'free' };
      plain.aspect = aspectKey && Object.prototype.hasOwnProperty.call(ASPECTS, aspectKey) ? aspectKey : 'free';
      return plain;
    }
    var fracRatio = ratio * (dispH / dispW);
    var bw = Number(base.w); if (!Number.isFinite(bw) || bw <= 0) bw = 1;
    var bh = Number(base.h); if (!Number.isFinite(bh) || bh <= 0) bh = 1;
    var bx = Number(base.x); if (!Number.isFinite(bx)) bx = 0;
    var by = Number(base.y); if (!Number.isFinite(by)) by = 0;
    var cx = bx + bw / 2;
    var cy = by + bh / 2;
    var w = bw;
    var h = w / fracRatio;
    if (h > 1) { h = 1; w = h * fracRatio; }
    if (w > 1) { w = 1; h = w / fracRatio; }
    if (h > 1) { h = 1; w = Math.min(1, h * fracRatio); }
    return clamp01Crop({ x: cx - w / 2, y: cy - h / 2, w: w, h: h, aspect: aspectKey });
  }

  function mapDisplayedPointToRaw(dx, dy, rawW, rawH, orientation) {
    switch (orientation | 0) {
      case 2: return { x: rawW - dx, y: dy };
      case 3: return { x: rawW - dx, y: rawH - dy };
      case 4: return { x: dx, y: rawH - dy };
      case 5: return { x: dy, y: dx };
      case 6: return { x: dy, y: rawH - dx };
      case 7: return { x: rawW - dy, y: rawH - dx };
      case 8: return { x: rawW - dy, y: dx };
      default: return { x: dx, y: dy };
    }
  }

  function displayedRectToRaw(dx, dy, dw, dh, rawW, rawH, orientation) {
    var pts = [
      mapDisplayedPointToRaw(dx, dy, rawW, rawH, orientation),
      mapDisplayedPointToRaw(dx + dw, dy, rawW, rawH, orientation),
      mapDisplayedPointToRaw(dx, dy + dh, rawW, rawH, orientation),
      mapDisplayedPointToRaw(dx + dw, dy + dh, rawW, rawH, orientation)
    ];
    var minX = Math.min(pts[0].x, pts[1].x, pts[2].x, pts[3].x);
    var maxX = Math.max(pts[0].x, pts[1].x, pts[2].x, pts[3].x);
    var minY = Math.min(pts[0].y, pts[1].y, pts[2].y, pts[3].y);
    var maxY = Math.max(pts[0].y, pts[1].y, pts[2].y, pts[3].y);
    return { x: minX, y: minY, w: maxX - minX, h: maxY - minY };
  }

  function exportSourceRect(rawW, rawH, orientation, crop) {
    var d = orientedSize(rawW, rawH, orientation || 1);
    var c = isActiveCrop(crop) ? normalizeCrop(crop) : { x: 0, y: 0, w: 1, h: 1 };
    var dx = c.x * d.w, dy = c.y * d.h, dw = c.w * d.w, dh = c.h * d.h;
    var raw = displayedRectToRaw(dx, dy, dw, dh, rawW, rawH, orientation || 1);
    var sx = Math.max(0, Math.min(rawW - 1, raw.x));
    var sy = Math.max(0, Math.min(rawH - 1, raw.y));
    var sw = Math.max(1, Math.min(rawW - sx, raw.w));
    var sh = Math.max(1, Math.min(rawH - sy, raw.h));
    return {
      sx: sx, sy: sy, sw: sw, sh: sh,
      outW: Math.max(1, Math.round(dw)),
      outH: Math.max(1, Math.round(dh)),
      displayedW: d.w,
      displayedH: d.h
    };
  }

  function exportPixelSize(rawW, rawH, orientation, crop) {
    var r = exportSourceRect(rawW, rawH, orientation, crop);
    return { width: r.outW, height: r.outH };
  }

  function displayedCropPixels(rawW, rawH, orientation, crop) {
    var d = orientedSize(rawW, rawH, orientation || 1);
    var c = isActiveCrop(crop) ? normalizeCrop(crop) : { x: 0, y: 0, w: 1, h: 1 };
    return { x: c.x * d.w, y: c.y * d.h, w: c.w * d.w, h: c.h * d.h, displayedW: d.w, displayedH: d.h };
  }

  function extOf(name) {
    var n = String(name || '');
    var i = n.lastIndexOf('.');
    return i >= 0 ? n.slice(i + 1).toLowerCase() : '';
  }

  function exportFormatForSource(name, mime) {
    var ext = extOf(name);
    var m = String(mime || '').toLowerCase();
    if (ext === 'png' || m === 'image/png') return { ext: 'png', mime: 'image/png' };
    return { ext: 'jpg', mime: 'image/jpeg', quality: 0.95 };
  }

  function cropDownloadName(originalName, formatExt) {
    var base = String(originalName || 'image').replace(/[\\/:*?"<>|]+/g, '_').trim() || 'image';
    base = base.replace(/\.[^.]+$/, '') || 'image';
    return base + '_crop.' + (formatExt || 'jpg');
  }

  function uniqueExportName(desired, existingNames) {
    var taken = Object.create(null);
    (existingNames || []).forEach(function (n) { taken[String(n).toLowerCase()] = true; });
    if (!taken[String(desired).toLowerCase()]) return desired;
    var m = String(desired).match(/^(.*?)(\.[^.]+)$/);
    var stem = m ? m[1] : String(desired);
    var ext = m ? m[2] : '';
    var base = stem.replace(/ \((\d+)\)$/, '');
    for (var n = 2; n < 10000; n++) {
      var candidate = base + ' (' + n + ')' + ext;
      if (!taken[candidate.toLowerCase()]) return candidate;
    }
    return desired;
  }

  function sidecarFileName(mediaFileName) {
    return String(mediaFileName || 'media') + '.sspcrop.json';
  }

  function samePath(a, b) {
    if (!a || !b) return false;
    return toWindows(String(a)).toLowerCase() === toWindows(String(b)).toLowerCase();
  }

  function isDefaultGrade(clr) {
    if (!clr) return true;
    var keys = Object.keys(CLR_DEF);
    for (var i = 0; i < keys.length; i++) {
      var k = keys[i];
      if (clr[k] == null) continue;
      if (Number(clr[k]) !== CLR_DEF[k]) return false;
    }
    return true;
  }

  function cropSidecarDocument(item, crop) {
    return {
      sspCropVersion: CROP_VERSION,
      app: 'SlideX',
      savedAt: new Date().toISOString(),
      source: {
        name: item && item.name || null,
        path: item && item.path || null,
        type: item && item.type || null,
        id: item && item.id || null
      },
      crop: normalizeCrop(crop)
    };
  }

  function parseCropSidecar(text) {
    var data;
    try { data = typeof text === 'string' ? JSON.parse(text) : text; } catch (e) { return null; }
    if (!data || data.sspCropVersion !== CROP_VERSION || !data.crop) return null;
    return normalizeCrop(data.crop);
  }

  /* Future Cast media server: stills can be cropped on the fly from these fractions.
     Videos cannot be cropped on the TV. */
  function castMediaHook(item) {
    if (!item) return { kind: 'none' };
    var crop = isActiveCrop(item.crop) ? normalizeCrop(item.crop) : null;
    if (item.type === 'video') {
      return {
        kind: 'video',
        path: item.path || null,
        crop: crop,
        cropOnTv: false,
        message: crop ? "Cropped videos can't be cropped on the TV." : null
      };
    }
    return { kind: 'still', path: item.path || null, crop: crop, cropOnTv: true };
  }

  function containRect(vpW, vpH, contentW, contentH) {
    var vw = Math.max(0, vpW), vh = Math.max(0, vpH);
    var cw = Math.max(1, contentW), ch = Math.max(1, contentH);
    var s = Math.min(vw / cw, vh / ch);
    if (!Number.isFinite(s) || s <= 0) s = 1;
    var w = cw * s, h = ch * s;
    return { x: (vw - w) / 2, y: (vh - h) / 2, w: w, h: h };
  }

  function clampView(view, vpW, vpH, contentW, contentH) {
    var zoom = Number(view && view.zoom);
    if (!Number.isFinite(zoom) || zoom < 1) zoom = 1;
    if (zoom > 10) zoom = 10;
    var base = containRect(vpW, vpH, contentW, contentH);
    var w = base.w * zoom;
    var h = base.h * zoom;
    var panX = Number(view && view.panX); if (!Number.isFinite(panX)) panX = 0;
    var panY = Number(view && view.panY); if (!Number.isFinite(panY)) panY = 0;
    var x = (vpW - w) / 2 + panX;
    var y = (vpH - h) / 2 + panY;
    if (w <= vpW + 0.5) x = (vpW - w) / 2;
    else {
      if (x > 0) x = 0;
      if (x < vpW - w) x = vpW - w;
    }
    if (h <= vpH + 0.5) y = (vpH - h) / 2;
    else {
      if (y > 0) y = 0;
      if (y < vpH - h) y = vpH - h;
    }
    return { zoom: zoom, panX: x - (vpW - w) / 2, panY: y - (vpH - h) / 2 };
  }

  function placedRect(view, vpW, vpH, contentW, contentH) {
    var c = clampView(view, vpW, vpH, contentW, contentH);
    var base = containRect(vpW, vpH, contentW, contentH);
    var w = base.w * c.zoom;
    var h = base.h * c.zoom;
    return {
      x: (vpW - w) / 2 + c.panX,
      y: (vpH - h) / 2 + c.panY,
      w: w, h: h, zoom: c.zoom, panX: c.panX, panY: c.panY, base: base
    };
  }

  function zoomToward(view, factor, pointerX, pointerY, vpW, vpH, contentW, contentH) {
    var cur = clampView(view, vpW, vpH, contentW, contentH);
    var rect = placedRect(cur, vpW, vpH, contentW, contentH);
    var zoom = cur.zoom * (Number(factor) > 0 ? Number(factor) : 1);
    if (!Number.isFinite(zoom)) zoom = cur.zoom;
    var nextUnclamped = { zoom: zoom, panX: 0, panY: 0 };
    if (!(rect.w > 0) || !(rect.h > 0)) return clampView(nextUnclamped, vpW, vpH, contentW, contentH);
    var relX = (pointerX - rect.x) / rect.w;
    var relY = (pointerY - rect.y) / rect.h;
    var base = containRect(vpW, vpH, contentW, contentH);
    var clampedZoom = clampView({ zoom: zoom, panX: 0, panY: 0 }, vpW, vpH, contentW, contentH).zoom;
    var nw = base.w * clampedZoom;
    var nh = base.h * clampedZoom;
    var newX = pointerX - relX * nw;
    var newY = pointerY - relY * nh;
    return clampView({
      zoom: clampedZoom,
      panX: newX - (vpW - nw) / 2,
      panY: newY - (vpH - nh) / 2
    }, vpW, vpH, contentW, contentH);
  }

  function wholeImageEndpoints(preset, zoomInt, panInt) {
    var zInt = Number.isFinite(zoomInt) ? zoomInt : 1;
    var pInt = Number.isFinite(panInt) ? panInt : 1;
    function z(s) { return 1 + ((s || 1) - 1) * zInt; }
    var from = { s: z(preset.fs), x: (preset.fx || 0) * pInt, y: (preset.fy || 0) * pInt };
    var to = { s: z(preset.ts), x: (preset.tx || 0) * pInt, y: (preset.ty || 0) * pInt };
    if (from.s <= to.s) {
      return { from: { s: 1, x: 0, y: 0 }, to: { s: Math.max(1, to.s), x: to.x, y: to.y } };
    }
    return { from: { s: Math.max(1, from.s), x: from.x, y: from.y }, to: { s: 1, x: 0, y: 0 } };
  }

  function toWindows(p) {
    var s = String(p || '');
    if (s.startsWith('//')) return '\\\\' + s.slice(2).replace(/\//g, '\\');
    if (s.startsWith('\\\\')) return '\\\\' + s.slice(2).replace(/\//g, '\\');
    return s.replace(/\//g, '\\');
  }

  function rewritePathPrefix(filePath, oldPrefix, newPrefix, opts) {
    if (!filePath || oldPrefix == null || newPrefix == null || oldPrefix === '') return null;
    var ci = !!(opts && opts.caseInsensitive);
    var file = toWindows(filePath);
    var oldP = toWindows(oldPrefix).replace(/\\+$/, '');
    var neu = toWindows(newPrefix).replace(/\\+$/, '');
    if (!oldP) return null;
    var fileCmp = ci ? file.toLowerCase() : file;
    var oldCmp = ci ? oldP.toLowerCase() : oldP;
    var rest;
    if (fileCmp === oldCmp) rest = '';
    else if (fileCmp.startsWith(oldCmp + '\\')) rest = file.slice(oldP.length + 1);
    else return null;
    var joined = rest ? (neu + '\\' + rest) : neu;
    if (String(newPrefix).indexOf('/') !== -1 && String(newPrefix).indexOf('\\') === -1) {
      return joined.replace(/\\/g, '/');
    }
    return joined;
  }

  function ancestorPrefixes(filePath) {
    var file = toWindows(filePath);
    var raw = file.split('\\');
    var prefixes = [];
    var n;
    if (raw.length >= 2 && raw[0] === '' && raw[1] === '') {
      for (n = raw.length - 1; n >= 3; n--) prefixes.push(raw.slice(0, n).join('\\'));
      return prefixes;
    }
    for (n = raw.length - 1; n >= 1; n--) {
      var pre = raw.slice(0, n).join('\\');
      if (pre) prefixes.push(pre);
    }
    return prefixes;
  }

  function planLocate(missingPaths, newRoot, existsFn, opts) {
    var paths = (missingPaths || []).filter(Boolean);
    var candidates = [];
    var seen = Object.create(null);
    paths.forEach(function (p) {
      ancestorPrefixes(p).forEach(function (pre) {
        var key = (opts && opts.caseInsensitive) ? pre.toLowerCase() : pre;
        if (!seen[key]) { seen[key] = true; candidates.push(pre); }
      });
    });
    var best = null;
    candidates.forEach(function (pre) {
      var rows = paths.map(function (p) {
        var rewritten = rewritePathPrefix(p, pre, newRoot, opts);
        if (!rewritten) return { path: p, rewritten: null, exists: false, outside: true };
        var exists = !!(existsFn && existsFn(rewritten));
        return { path: p, rewritten: rewritten, exists: exists, outside: false };
      });
      var found = rows.filter(function (r) { return r.exists; }).length;
      var eligible = rows.filter(function (r) { return !r.outside; }).length;
      if (!best || found > best.found || (found === best.found && pre.length > best.prefix.length)) {
        best = { prefix: pre, found: found, eligible: eligible, rows: rows };
      }
    });
    return best;
  }

  function imageUpdatesFromItem(it) {
    return {
      clr: it.clr,
      rotation: it.rotation,
      mirrorX: it.mirrorX,
      mirrorY: it.mirrorY,
      zoom: it.zoom,
      panX: it.panX,
      panY: it.panY,
      customDur: it.customDur,
      kbPreset: it.kbPreset,
      crop: it.crop || null
    };
  }

  function buildPlaylistDocument(opts) {
    var o = opts || {};
    var items = (o.items || []).map(function (it) {
      if (it.type === 'folder') {
        return { type: 'folder', id: it.id, name: it.name, open: it.open !== false };
      }
      return {
        id: it.id,
        type: it.type,
        name: it.name,
        path: it.path || undefined,
        imageUpdates: imageUpdatesFromItem(it)
      };
    });
    var doc = { sspVersion: PLAYLIST_SSP_VERSION, version: 2, savedAt: o.savedAt || Date.now(), items: items };
    if (o.name) doc.name = o.name;
    if (o.kb) doc.kb = o.kb;
    return doc;
  }

  function normalizePlaylist(data) {
    if (!data || typeof data !== 'object' || !Array.isArray(data.items)) {
      throw new Error('invalid playlist');
    }
    var sspVersion = data.sspVersion;
    if (!sspVersion && data.version === 2) sspVersion = 1;
    if (!sspVersion) sspVersion = 1;
    return {
      sspVersion: sspVersion,
      version: data.version || 2,
      name: data.name || null,
      savedAt: data.savedAt || null,
      kb: data.kb || null,
      items: data.items.map(function (entry) { return entry; })
    };
  }

  function restoreItemFields(entry) {
    if (!entry || entry.type === 'folder') return entry;
    var u = entry.imageUpdates !== undefined ? entry.imageUpdates : {};
    return {
      id: entry.id,
      type: entry.type,
      name: entry.name,
      path: entry.path,
      clr: u.clr,
      rotation: u.rotation,
      mirrorX: u.mirrorX,
      mirrorY: u.mirrorY,
      zoom: u.zoom,
      panX: u.panX,
      panY: u.panY,
      customDur: u.customDur,
      kbPreset: u.kbPreset,
      crop: u.crop || null
    };
  }

  function markMissing(items, pathExists) {
    var missing = 0;
    var out = (items || []).map(function (it) {
      if (!it || it.type === 'folder') return it;
      var path = it.path;
      var gone = !path || !pathExists(path);
      if (!gone) {
        var copy = {};
        Object.keys(it).forEach(function (k) { copy[k] = it[k]; });
        copy.missing = false;
        if (copy.unavailable && copy.missingWas) copy.unavailable = false;
        return copy;
      }
      missing++;
      var miss = {};
      Object.keys(it).forEach(function (k) { miss[k] = it[k]; });
      miss.missing = true;
      miss.unavailable = true;
      return miss;
    });
    return { items: out, missing: missing };
  }

  function slideshowItems(items) {
    return (items || []).filter(function (it) {
      return it && it.type !== 'folder' && !it.unavailable && !it.missing;
    });
  }

  function isAltBackspaceReset(e) {
    return !!(e && e.altKey && e.key === 'Backspace' && !e.ctrlKey && !e.metaKey && !e.shiftKey);
  }

  function willRemovePlaylistItem(e) {
    if (!e || (e.key !== 'Backspace' && e.key !== 'Delete')) return false;
    if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return false;
    return true;
  }

  function playlistCountAfterKey(count, e) {
    if (willRemovePlaylistItem(e)) return Math.max(0, count - 1);
    return count;
  }

  function isHueUpKey(key) { return key === '0'; }
  function isZoomInKey(key) { return key === '+' || key === '='; }
  function isZoomOutKey(key) { return key === '-'; }
  function isResetPanKey(key) { return key === 'p'; }

  /* JPEG APP1 Exif with Orientation=1 and DateTimeOriginal. Dates are baked as ASCII. */
  function buildExifApp1(dateStr) {
    var date = String(dateStr || '');
    if (!/^\d{4}:\d{2}:\d{2} \d{2}:\d{2}:\d{2}$/.test(date)) return null;
    var ascii = [];
    for (var i = 0; i < date.length; i++) ascii.push(date.charCodeAt(i));
    ascii.push(0);
    // TIFF layout (little-endian)
    // 0: header 8 bytes
    // 8: IFD0 count + 3 entries + next
    // then date string, then Exif IFD, then second date string
    var ifd0 = 8;
    var date0 = ifd0 + 2 + 3 * 12 + 4;
    var exifIfd = date0 + 20;
    var date1 = exifIfd + 2 + 2 * 12 + 4;
    var end = date1 + 20;
    var b = new Uint8Array(end);
    b[0] = 0x49; b[1] = 0x49; b[2] = 0x2a; b[3] = 0x00;
    b[4] = 8; // offset to IFD0
    function u16(off, v) { b[off] = v & 255; b[off + 1] = (v >> 8) & 255; }
    function u32(off, v) { u16(off, v & 65535); u16(off + 2, (v >>> 16) & 65535); }
    u16(ifd0, 3);
    // Orientation SHORT = 1
    u16(ifd0 + 2, 0x0112); u16(ifd0 + 4, 3); u32(ifd0 + 6, 1); u16(ifd0 + 10, 1);
    // DateTime ASCII
    u16(ifd0 + 14, 0x0132); u16(ifd0 + 16, 2); u32(ifd0 + 18, 20); u32(ifd0 + 22, date0);
    // Exif IFD pointer
    u16(ifd0 + 26, 0x8769); u16(ifd0 + 28, 4); u32(ifd0 + 30, 1); u32(ifd0 + 34, exifIfd);
    u32(ifd0 + 38, 0);
    b.set(ascii, date0);
    u16(exifIfd, 2);
    u16(exifIfd + 2, 0x9003); u16(exifIfd + 4, 2); u32(exifIfd + 6, 20); u32(exifIfd + 10, date1);
    u16(exifIfd + 14, 0x9004); u16(exifIfd + 16, 2); u32(exifIfd + 18, 20); u32(exifIfd + 22, date1);
    u32(exifIfd + 26, 0);
    b.set(ascii, date1);
    var app1 = new Uint8Array(b.length + 10);
    app1[0] = 0xff; app1[1] = 0xe1;
    var len = b.length + 8;
    app1[2] = (len >> 8) & 255; app1[3] = len & 255;
    app1[4] = 0x45; app1[5] = 0x78; app1[6] = 0x69; app1[7] = 0x66; app1[8] = 0; app1[9] = 0;
    app1.set(b, 10);
    return app1;
  }

  function readJpegDate(bytes) {
    if (!bytes || bytes.length < 4 || bytes[0] !== 0xff || bytes[1] !== 0xd8) return null;
    var i = 2;
    while (i + 4 < bytes.length) {
      if (bytes[i] !== 0xff) { i++; continue; }
      var marker = bytes[i + 1];
      if (marker === 0xda || marker === 0xd9) break;
      if (marker === 0x00 || marker === 0x01 || (marker >= 0xd0 && marker <= 0xd9)) { i += 2; continue; }
      var segLen = (bytes[i + 2] << 8) | bytes[i + 3];
      if (segLen < 2 || i + 2 + segLen > bytes.length) break;
      if (marker === 0xe1 && segLen > 8) {
        if (bytes[i + 4] === 0x45 && bytes[i + 5] === 0x78 && bytes[i + 6] === 0x69 && bytes[i + 7] === 0x66) {
          var tiff = i + 10;
          var found = readTiffDate(bytes, tiff, i + 2 + segLen);
          if (found) return found;
        }
      }
      i += 2 + segLen;
    }
    return null;
  }

  function readTiffDate(bytes, tiff, end) {
    if (tiff + 8 > end) return null;
    var le = bytes[tiff] === 0x49 && bytes[tiff + 1] === 0x49;
    function u16(off) {
      if (off + 1 >= end) return 0;
      return le ? (bytes[off] | (bytes[off + 1] << 8)) : ((bytes[off] << 8) | bytes[off + 1]);
    }
    function u32(off) {
      if (off + 3 >= end) return 0;
      return le
        ? (bytes[off] | (bytes[off + 1] << 8) | (bytes[off + 2] << 16) | (bytes[off + 3] << 24)) >>> 0
        : ((bytes[off] << 24) | (bytes[off + 1] << 16) | (bytes[off + 2] << 8) | bytes[off + 3]) >>> 0;
    }
    var ifd = tiff + u32(tiff + 4);
    var date = null;
    var guard = 0;
    while (ifd && guard++ < 8) {
      if (ifd + 2 > end) break;
      var count = u16(ifd);
      var exifPtr = 0;
      for (var e = 0; e < count; e++) {
        var ent = ifd + 2 + e * 12;
        if (ent + 12 > end) break;
        var tag = u16(ent);
        var typ = u16(ent + 2);
        var cnt = u32(ent + 4);
        var valOff = cnt * (typ === 2 ? 1 : 1) > 4 ? tiff + u32(ent + 8) : ent + 8;
        if (tag === 0x0132 || tag === 0x9003 || tag === 0x9004) {
          var s = readAscii(bytes, valOff, Math.min(cnt, 32), end);
          if (s && /^\d{4}:\d{2}:\d{2} \d{2}:\d{2}:\d{2}/.test(s)) date = s.slice(0, 19);
        }
        if (tag === 0x8769) exifPtr = tiff + u32(ent + 8);
      }
      var next = u32(ifd + 2 + count * 12);
      if (!date && exifPtr) { ifd = exifPtr; continue; }
      if (date) return date;
      if (!next) break;
      ifd = tiff + next;
    }
    return date;
  }

  function readAscii(bytes, off, len, end) {
    var s = '';
    for (var i = 0; i < len && off + i < end && off + i < bytes.length; i++) {
      var c = bytes[off + i];
      if (c === 0) break;
      s += String.fromCharCode(c);
    }
    return s;
  }

  function readLatin1(bytes, off, len, end) {
    var s = '';
    for (var i = 0; i < len && off + i < end && off + i < bytes.length; i++) {
      s += String.fromCharCode(bytes[off + i]);
    }
    return s;
  }

  function firstExifDate(text) {
    var re = /\d{4}:\d{2}:\d{2} \d{2}:\d{2}:\d{2}/;
    var m = re.exec(text || '');
    return m ? m[0] : null;
  }

  function injectJpegExif(jpegBytes, dateStr) {
    var app1 = buildExifApp1(dateStr);
    if (!app1 || !jpegBytes || jpegBytes.length < 2 || jpegBytes[0] !== 0xff || jpegBytes[1] !== 0xd8) return jpegBytes;
    var parts = [jpegBytes.subarray(0, 2)];
    var i = 2;
    while (i + 4 < jpegBytes.length) {
      if (jpegBytes[i] !== 0xff) break;
      var marker = jpegBytes[i + 1];
      if (marker === 0xda || marker === 0xd9) break;
      if (marker === 0x00 || marker === 0x01 || (marker >= 0xd0 && marker <= 0xd9)) break;
      var segLen = (jpegBytes[i + 2] << 8) | jpegBytes[i + 3];
      if (segLen < 2 || i + 2 + segLen > jpegBytes.length) break;
      var isExif = marker === 0xe1 && segLen > 8
        && jpegBytes[i + 4] === 0x45 && jpegBytes[i + 5] === 0x78 && jpegBytes[i + 6] === 0x69 && jpegBytes[i + 7] === 0x66;
      if (!isExif) parts.push(jpegBytes.subarray(i, i + 2 + segLen));
      i += 2 + segLen;
    }
    parts.push(app1);
    parts.push(jpegBytes.subarray(i));
    var total = parts.reduce(function (n, p) { return n + p.length; }, 0);
    var out = new Uint8Array(total);
    var o = 0;
    parts.forEach(function (p) { out.set(p, o); o += p.length; });
    return out;
  }

  function readPngDate(bytes) {
    if (!bytes || bytes.length < 8) return null;
    var sig = [137, 80, 78, 71, 13, 10, 26, 10];
    for (var s = 0; s < 8; s++) if (bytes[s] !== sig[s]) return null;
    var i = 8;
    while (i + 8 <= bytes.length) {
      var len = (bytes[i] << 24) | (bytes[i + 1] << 16) | (bytes[i + 2] << 8) | bytes[i + 3];
      len = len >>> 0;
      var type = String.fromCharCode(bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]);
      if (i + 12 + len > bytes.length) break;
      if (type === 'tEXt' || type === 'iTXt' || type === 'zTXt' || type === 'eXIf') {
        var text = readLatin1(bytes, i + 8, len, i + 8 + len);
        var found = firstExifDate(text);
        if (found) return found;
      }
      if (type === 'IEND') break;
      i += 12 + len;
    }
    return null;
  }

  function crc32(buf) {
    var c = ~0;
    for (var n = 0; n < buf.length; n++) {
      c ^= buf[n];
      for (var k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1));
    }
    return ~c >>> 0;
  }

  function injectPngDate(pngBytes, dateStr) {
    if (!/^\d{4}:\d{2}:\d{2} \d{2}:\d{2}:\d{2}$/.test(dateStr || '')) return pngBytes;
    var sigOk = pngBytes && pngBytes.length > 8 && pngBytes[0] === 137 && pngBytes[1] === 80;
    if (!sigOk) return pngBytes;
    var keyword = 'DateTimeOriginal';
    var payload = [];
    for (var i = 0; i < keyword.length; i++) payload.push(keyword.charCodeAt(i));
    payload.push(0);
    for (var j = 0; j < dateStr.length; j++) payload.push(dateStr.charCodeAt(j));
    var data = new Uint8Array(payload);
    var type = [116, 69, 88, 116]; // tEXt
    var chunk = new Uint8Array(12 + data.length);
    chunk[0] = (data.length >>> 24) & 255;
    chunk[1] = (data.length >>> 16) & 255;
    chunk[2] = (data.length >>> 8) & 255;
    chunk[3] = data.length & 255;
    chunk.set(type, 4);
    chunk.set(data, 8);
    var crcBuf = new Uint8Array(4 + data.length);
    crcBuf.set(type, 0);
    crcBuf.set(data, 4);
    var crc = crc32(crcBuf);
    chunk[8 + data.length] = (crc >>> 24) & 255;
    chunk[9 + data.length] = (crc >>> 16) & 255;
    chunk[10 + data.length] = (crc >>> 8) & 255;
    chunk[11 + data.length] = crc & 255;
    // Insert before IEND
    var iend = -1;
    var p = 8;
    while (p + 8 <= pngBytes.length) {
      var len = ((pngBytes[p] << 24) | (pngBytes[p + 1] << 16) | (pngBytes[p + 2] << 8) | pngBytes[p + 3]) >>> 0;
      var t = String.fromCharCode(pngBytes[p + 4], pngBytes[p + 5], pngBytes[p + 6], pngBytes[p + 7]);
      if (t === 'IEND') { iend = p; break; }
      p += 12 + len;
    }
    if (iend < 0) return pngBytes;
    var out = new Uint8Array(pngBytes.length + chunk.length);
    out.set(pngBytes.subarray(0, iend), 0);
    out.set(chunk, iend);
    out.set(pngBytes.subarray(iend), iend + chunk.length);
    return out;
  }

  function findLooseDate(bytes) {
    if (!bytes || !bytes.length) return null;
    var s = '';
    var limit = Math.min(bytes.length, 8 * 1024 * 1024);
    for (var i = 0; i < limit; i++) {
      var c = bytes[i];
      s += (c >= 32 && c < 127) ? String.fromCharCode(c) : '\n';
    }
    return firstExifDate(s);
  }

  function readDateTaken(bytes) {
    return readJpegDate(bytes) || readPngDate(bytes) || findLooseDate(bytes);
  }

  function copyDateTaken(sourceBytes, destBytes) {
    var date = readDateTaken(sourceBytes);
    if (!date || !destBytes || destBytes.length < 2) return destBytes;
    if (destBytes[0] === 0xff && destBytes[1] === 0xd8) return injectJpegExif(destBytes, date);
    if (destBytes[0] === 137 && destBytes[1] === 80) return injectPngDate(destBytes, date);
    return destBytes;
  }

  function joinPath(dir, name) {
    var d = String(dir || '');
    var sep = d.indexOf('\\') !== -1 ? '\\' : '/';
    if (!d) return name;
    if (d.endsWith('\\') || d.endsWith('/')) return d + name;
    return d + sep + name;
  }

  root.SlideXCore = {
    CROP_VERSION: CROP_VERSION,
    PLAYLIST_SSP_VERSION: PLAYLIST_SSP_VERSION,
    ASPECTS: ASPECTS,
    CLR_DEF: CLR_DEF,
    orientedSize: orientedSize,
    normalizeCrop: normalizeCrop,
    isActiveCrop: isActiveCrop,
    applyAspect: applyAspect,
    displayedRectToRaw: displayedRectToRaw,
    exportSourceRect: exportSourceRect,
    exportPixelSize: exportPixelSize,
    displayedCropPixels: displayedCropPixels,
    exportFormatForSource: exportFormatForSource,
    cropDownloadName: cropDownloadName,
    uniqueExportName: uniqueExportName,
    sidecarFileName: sidecarFileName,
    samePath: samePath,
    isDefaultGrade: isDefaultGrade,
    cropSidecarDocument: cropSidecarDocument,
    parseCropSidecar: parseCropSidecar,
    castMediaHook: castMediaHook,
    containRect: containRect,
    clampView: clampView,
    placedRect: placedRect,
    zoomToward: zoomToward,
    wholeImageEndpoints: wholeImageEndpoints,
    rewritePathPrefix: rewritePathPrefix,
    ancestorPrefixes: ancestorPrefixes,
    planLocate: planLocate,
    buildPlaylistDocument: buildPlaylistDocument,
    normalizePlaylist: normalizePlaylist,
    restoreItemFields: restoreItemFields,
    markMissing: markMissing,
    slideshowItems: slideshowItems,
    isAltBackspaceReset: isAltBackspaceReset,
    willRemovePlaylistItem: willRemovePlaylistItem,
    playlistCountAfterKey: playlistCountAfterKey,
    isHueUpKey: isHueUpKey,
    isZoomInKey: isZoomInKey,
    isZoomOutKey: isZoomOutKey,
    isResetPanKey: isResetPanKey,
    readJpegDate: readJpegDate,
    injectJpegExif: injectJpegExif,
    readPngDate: readPngDate,
    injectPngDate: injectPngDate,
    readDateTaken: readDateTaken,
    copyDateTaken: copyDateTaken,
    joinPath: joinPath
  };
})(typeof globalThis !== 'undefined' ? globalThis : this);
