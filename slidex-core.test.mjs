import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const context = { console, Uint8Array, Date, Object, Math, String, Number, Array };
context.globalThis = context;
vm.runInNewContext(fs.readFileSync(new URL('./slidex-core.js', import.meta.url), 'utf8'), context);
const C = context.SlideXCore;

const KB = [
  { fs: 1.0, ts: 1.15, fx: 0, fy: 0, tx: -3, ty: -3 },
  { fs: 1.15, ts: 1.0, fx: -3, fy: -3, tx: 0, ty: 0 },
  { fs: 1.0, ts: 1.12, fx: 3, fy: 3, tx: -2, ty: -2 },
  { fs: 1.12, ts: 1.0, fx: -2, fy: 2, tx: 2, ty: -2 },
  { fs: 1.0, ts: 1.1, fx: -4, fy: 0, tx: 4, ty: 0 }
];
const ASPECTS = ['free', '16:9', '4:3', '1:1', '9:16'];

test('export size follows the displayed frame for EXIF orientations 1–8', () => {
  const rawW = 4000, rawH = 3001;
  const crop = { x: 0.1, y: 0.2, w: 0.3333, h: 0.4, aspect: 'free' };
  for (let o = 1; o <= 8; o++) {
    const d = C.orientedSize(rawW, rawH, o);
    if (o >= 5) { assert.equal(d.w, rawH); assert.equal(d.h, rawW); }
    else { assert.equal(d.w, rawW); assert.equal(d.h, rawH); }
    const size = C.exportPixelSize(rawW, rawH, o, crop);
    assert.ok(Math.abs(size.width - crop.w * d.w) < 1, `w ori ${o}`);
    assert.ok(Math.abs(size.height - crop.h * d.h) < 1, `h ori ${o}`);
    const full = C.exportPixelSize(rawW, rawH, o, { x: 0, y: 0, w: 1, h: 1 });
    assert.equal(full.width, d.w);
    assert.equal(full.height, d.h);
    const raw = C.displayedRectToRaw(0, 0, d.w, d.h, rawW, rawH, o);
    assert.ok(Math.abs(raw.w - rawW) < 1e-6 && Math.abs(raw.h - rawH) < 1e-6, `raw ori ${o}`);
  }
});

test('aspect presets lock the displayed crop for every EXIF orientation', () => {
  const frames = [];
  for (let o = 1; o <= 8; o++) frames.push([4000, 3000, o]);
  for (const [rawW, rawH, o] of frames) {
    const d = C.orientedSize(rawW, rawH, o);
    for (const aspect of ASPECTS) {
      const crop = C.applyAspect({ x: 0, y: 0, w: 1, h: 1 }, aspect, d.w, d.h);
      assert.ok(crop.x >= -1e-9 && crop.y >= -1e-9);
      assert.ok(crop.x + crop.w <= 1 + 1e-6 && crop.y + crop.h <= 1 + 1e-6);
      assert.equal(crop.aspect, aspect);
      if (aspect === 'free') continue;
      const px = C.displayedCropPixels(rawW, rawH, o, crop);
      const got = px.w / px.h;
      const want = C.ASPECTS[aspect];
      assert.ok(Math.abs(got - want) / want < 0.02, `${aspect} ori ${o} got ${got}`);
      const size = C.exportPixelSize(rawW, rawH, o, crop);
      assert.ok(Math.abs(size.width - px.w) < 1);
      assert.ok(Math.abs(size.height - px.h) < 1);
    }
  }
});

test('portrait orientation 6 maps a displayed top crop onto the stored pixels', () => {
  // 4000x3000 stored, orientation 6 displays as 3000x4000.
  const crop = { x: 0, y: 0, w: 1, h: 0.5, aspect: 'free' };
  const size = C.exportPixelSize(4000, 3000, 6, crop);
  assert.equal(size.width, 3000);
  assert.equal(size.height, 2000);
  const src = C.exportSourceRect(4000, 3000, 6, crop);
  assert.ok(Math.abs(src.sw - 2000) < 1);
  assert.ok(Math.abs(src.sh - 3000) < 1);
});

test('unique export names add (2) and never reuse the original', () => {
  assert.equal(C.cropDownloadName('Vacation.jpg', 'jpg'), 'Vacation_crop.jpg');
  assert.equal(C.cropDownloadName('shot.PNG', 'png'), 'shot_crop.png');
  assert.equal(C.uniqueExportName('Vacation_crop.jpg', []), 'Vacation_crop.jpg');
  assert.equal(C.uniqueExportName('Vacation_crop.jpg', ['Vacation_crop.jpg']), 'Vacation_crop (2).jpg');
  assert.equal(
    C.uniqueExportName('Vacation_crop.jpg', ['Vacation_crop.jpg', 'Vacation_crop (2).jpg']),
    'Vacation_crop (3).jpg'
  );
  assert.equal(C.uniqueExportName('Vacation_crop.jpg', ['vacation_crop.jpg']), 'Vacation_crop (2).jpg');
  assert.notEqual(C.cropDownloadName('Vacation.jpg', 'jpg'), 'Vacation.jpg');
  assert.equal(C.sidecarFileName('Vacation.jpg'), 'Vacation.jpg.sspcrop.json');
  assert.equal(C.samePath('G:\\Photos\\a.jpg', 'g:/photos/a.jpg'), true);
  assert.equal(C.samePath('G:\\Photos\\a.jpg', 'G:\\Photos\\a_crop.jpg'), false);
});

test('HEIC and JPEG export as JPEG, PNG stays PNG', () => {
  assert.equal(C.exportFormatForSource('a.heic', '').ext, 'jpg');
  assert.equal(C.exportFormatForSource('a.HEIC', 'image/heic').quality, 0.95);
  assert.equal(C.exportFormatForSource('a.jpg', 'image/jpeg').ext, 'jpg');
  assert.equal(C.exportFormatForSource('a.png', 'image/png').ext, 'png');
});

test('broken crop sidecars are ignored', () => {
  assert.equal(C.parseCropSidecar('not json'), null);
  assert.equal(C.parseCropSidecar('{"sspCropVersion":1}'), null);
  assert.equal(C.parseCropSidecar('{"sspCropVersion":2,"crop":{"x":0,"y":0,"w":1,"h":1}}'), null);
  const ok = C.parseCropSidecar('{"sspCropVersion":1,"crop":{"x":0.1,"y":0.2,"w":0.5,"h":0.4,"aspect":"16:9"}}');
  assert.equal(ok.aspect, '16:9');
  assert.ok(Math.abs(ok.w - 0.5) < 1e-9);
});

test('playlist round trip keeps order, crop, grade, timing, and Ken Burns', () => {
  const doc = C.buildPlaylistDocument({
    name: 'Weekend',
    savedAt: 1700000000000,
    kb: { enabled: true, zoomInt: 4, panInt: 4, direction: 'mix', easing: 'ease' },
    items: [
      { type: 'folder', id: 'f1', name: 'Day', open: true },
      {
        id: 'a', type: 'image', name: 'portrait.jpg', path: 'G:\\Photos\\portrait.jpg',
        clr: { b: 110, c: 100, s: 120, h: 5, hl: 0, sh: 0, g: 100, shp: 110 },
        customDur: 8000, kbPreset: 2, zoom: 1.05, panX: 0, panY: 0, rotation: 0,
        crop: { x: 0.1, y: 0.05, w: 0.8, h: 0.9, aspect: '4:3' }
      },
      {
        id: 'b', type: 'video', name: 'clip.mp4', path: 'G:\\Photos\\clip.mp4',
        clr: C.CLR_DEF, customDur: undefined, kbPreset: -1, crop: null
      }
    ]
  });
  assert.equal(doc.sspVersion, 1);
  const back = C.normalizePlaylist(JSON.parse(JSON.stringify(doc)));
  assert.equal(back.name, 'Weekend');
  assert.equal(back.items[0].type, 'folder');
  assert.deepEqual(back.items.map(it => it.path || it.name), ['Day', 'G:\\Photos\\portrait.jpg', 'G:\\Photos\\clip.mp4']);
  const a = C.restoreItemFields(back.items[1]);
  assert.equal(a.customDur, 8000);
  assert.equal(a.kbPreset, 2);
  assert.equal(a.clr.b, 110);
  assert.equal(a.clr.shp, 110);
  assert.ok(Math.abs(a.crop.w - 0.8) < 1e-9);
  assert.equal(a.crop.aspect, '4:3');
  const b = C.restoreItemFields(back.items[2]);
  assert.equal(b.kbPreset, -1);
  assert.equal(b.crop, null);
});

test('legacy SlideX and SlideShowX playlist JSON loads', () => {
  const slideshowx = {
    version: 2,
    items: [{
      id: 'old', type: 'image', name: 'lake.jpg', path: 'D:\\lake.jpg',
      imageUpdates: { clr: { b: 90 }, customDur: 3000, kbPreset: 0 }
    }]
  };
  const legacy = C.normalizePlaylist(slideshowx);
  assert.equal(legacy.sspVersion, 1);
  const item = C.restoreItemFields(legacy.items[0]);
  assert.equal(item.path, 'D:\\lake.jpg');
  assert.equal(item.customDur, 3000);
  assert.equal(item.kbPreset, 0);
  assert.equal(item.clr.b, 90);

  const slidex = {
    sspVersion: 1, version: 2, name: 'Trip',
    items: [{ type: 'image', name: 'a.jpg', path: 'G:\\a.jpg', imageUpdates: { crop: { x: 0, y: 0, w: 0.5, h: 1, aspect: 'free' } } }]
  };
  const cur = C.normalizePlaylist(slidex);
  assert.equal(cur.sspVersion, 1);
  assert.equal(C.restoreItemFields(cur.items[0]).crop.w, 0.5);
  assert.throws(() => C.normalizePlaylist({ name: 'nope' }));
});

test('missing items are marked and skipped without dropping the playlist', () => {
  const items = [
    { type: 'folder', name: 'F' },
    { type: 'image', name: 'keep.jpg', path: 'G:\\keep.jpg' },
    { type: 'image', name: 'gone.jpg', path: 'G:\\gone.jpg' },
    { type: 'video', name: 'gone.mp4', path: 'G:\\gone.mp4' }
  ];
  const marked = C.markMissing(items, (p) => p === 'G:\\keep.jpg');
  assert.equal(marked.missing, 2);
  assert.equal(marked.items.length, 4);
  assert.equal(marked.items[0].type, 'folder');
  assert.equal(marked.items[1].missing, false);
  assert.equal(marked.items[2].missing, true);
  assert.equal(marked.items[2].unavailable, true);
  assert.deepEqual(C.slideshowItems(marked.items).map(it => it.name), ['keep.jpg']);
});

test('prefix rewrite: drive to UNC, UNC to UNC, case, and segment boundaries', () => {
  const ci = { caseInsensitive: true };
  assert.equal(
    C.rewritePathPrefix('G:\\Photos\\Vacation\\a.jpg', 'G:\\', '\\\\AX03\\Archive01', ci),
    '\\\\AX03\\Archive01\\Photos\\Vacation\\a.jpg'
  );
  assert.equal(
    C.rewritePathPrefix('\\\\AX03\\G\\Photos\\a.jpg', '\\\\AX03\\G', '\\\\AX03\\Archive01', ci),
    '\\\\AX03\\Archive01\\Photos\\a.jpg'
  );
  assert.equal(
    C.rewritePathPrefix('\\\\old\\share\\b.jpg', '\\\\old\\share', '\\\\AX03\\Archive01', ci),
    '\\\\AX03\\Archive01\\b.jpg'
  );
  assert.equal(
    C.rewritePathPrefix('g:\\photos\\A.JPG', 'G:\\Photos', '\\\\AX03\\Archive01', ci),
    '\\\\AX03\\Archive01\\A.JPG'
  );
  assert.equal(C.rewritePathPrefix('G:\\Photos2\\a.jpg', 'G:\\Photos', '\\\\AX03\\Archive01', ci), null);
  assert.equal(C.rewritePathPrefix('G:\\Photos\\a.jpg', 'G:\\Photo', 'D:\\X', ci), null);
  assert.equal(C.rewritePathPrefix('D:\\Other\\b.jpg', 'G:\\', '\\\\AX03\\Archive01', ci), null);
});

test('playlist name keys ignore case while the saved display name stays as typed', () => {
  assert.equal(C.playlistNameKey('Beach'), C.playlistNameKey('beach'));
  assert.equal(C.playlistNameKey(' Beach '), C.playlistNameKey('BEACH'));
  assert.equal(C.playlistNamesMatch('Beach', 'beach'), true);
  assert.equal(C.playlistNamesMatch('Beach', 'Shore'), false);
  const doc = C.buildPlaylistDocument({ name: 'Beach', items: [{ id: '1', type: 'image', name: 'a.jpg', path: 'G:\\a.jpg' }] });
  assert.equal(doc.name, 'Beach');
});

test('locate probing stops at the first matching parent', () => {
  const n = 40;
  const files = [];
  for (let i = 0; i < n; i++) files.push('\\\\AX03\\Archive01\\Show\\Reel\\Take\\Clip' + i + '.mp4');
  const ancestors = C.ancestorPrefixes(files[0]).length;
  assert.ok(ancestors > 1);
  let probes = 0;
  const exists = (p) => {
    probes++;
    return /\\zb23\\media\\clip\d+\.mp4$/i.test(String(p));
  };
  const plan = C.planLocate(files, '\\\\ZB23\\Media', exists, { caseInsensitive: true });
  assert.equal(plan.found, n);
  assert.equal(probes, n);
  assert.ok(n * ancestors > probes);
});

test('locate plan only rewrites paths that exist and leaves outsiders', () => {
  const missing = [
    'G:\\Photos\\a.jpg',
    'G:\\Photos\\b.jpg',
    'D:\\Other\\c.jpg',
    'G:\\Photos2\\d.jpg'
  ];
  const have = new Set([
    '\\\\ax03\\archive01\\photos\\a.jpg',
    '\\\\ax03\\archive01\\photos\\b.jpg'
  ]);
  const exists = (p) => have.has(String(p).toLowerCase());
  const plan = C.planLocate(missing, '\\\\AX03\\Archive01', exists, { caseInsensitive: true });
  assert.equal(plan.found, 2);
  assert.ok(plan.eligible >= 2);
  const byPath = Object.fromEntries(plan.rows.map(r => [r.path, r]));
  assert.equal(byPath['G:\\Photos\\a.jpg'].exists, true);
  assert.equal(byPath['G:\\Photos\\b.jpg'].exists, true);
  assert.equal(byPath['G:\\Photos\\a.jpg'].rewritten, '\\\\AX03\\Archive01\\Photos\\a.jpg');
  assert.equal(byPath['D:\\Other\\c.jpg'].outside, true);
  assert.equal(byPath['G:\\Photos2\\d.jpg'].exists, false);
  assert.equal(
    C.rewritePathPrefix('G:\\Photos2\\d.jpg', 'G:\\Photos', '\\\\AX03\\Archive01', { caseInsensitive: true }),
    null
  );
});

test('contain and clamp: portrait, landscape, panorama, and a crop', () => {
  const vp = { w: 1920, h: 1080 };
  const portrait = C.containRect(vp.w, vp.h, 1080, 1920);
  assert.ok(Math.abs(portrait.h - 1080) < 1e-6);
  assert.ok(portrait.w < vp.w - 10);
  assert.ok(portrait.x > 0);

  const landscape = C.containRect(vp.w, vp.h, 4000, 3000);
  assert.ok(Math.abs(landscape.w - 1440) < 1e-6 || Math.abs(landscape.h - 1080) < 1e-6);
  assert.ok(landscape.w <= vp.w + 1e-6 && landscape.h <= vp.h + 1e-6);

  const pano = C.containRect(vp.w, vp.h, 6000, 800);
  assert.ok(Math.abs(pano.w - 1920) < 1e-6);
  assert.ok(pano.h < vp.h - 10);

  const square = C.containRect(vp.w, vp.h, 1000, 1000);
  assert.ok(Math.abs(square.w - square.h) < 1e-6);
  assert.ok(square.h <= vp.h + 1e-6);

  const floored = C.clampView({ zoom: 0.2, panX: 400, panY: 400 }, vp.w, vp.h, 1080, 1920);
  assert.equal(floored.zoom, 1);
  assert.ok(Math.abs(floored.panX) < 1e-6 && Math.abs(floored.panY) < 1e-6);

  const dragged = C.clampView({ zoom: 3, panX: 99999, panY: -99999 }, vp.w, vp.h, 6000, 800);
  const rect = C.placedRect(dragged, vp.w, vp.h, 6000, 800);
  assert.ok(rect.x <= 0.01);
  assert.ok(rect.x + rect.w >= vp.w - 0.01);
  assert.equal(dragged.zoom, 3);

  const portraitZoom = C.clampView({ zoom: 2, panX: 5000, panY: 5000 }, vp.w, vp.h, 1080, 1920);
  const prect = C.placedRect(portraitZoom, vp.w, vp.h, 1080, 1920);
  assert.ok(prect.y <= 0.01 && prect.y + prect.h >= vp.h - 0.01);
  // Still pillarboxed horizontally, so it stays centered on X.
  assert.ok(Math.abs(prect.x - (vp.w - prect.w) / 2) < 1e-4);
});

test('zoom toward the pointer does not go below the whole image', () => {
  const before = { zoom: 1, panX: 0, panY: 0 };
  const zoomed = C.zoomToward(before, 2, 100, 540, 1920, 1080, 1080, 1920);
  assert.ok(zoomed.zoom >= 1.9 && zoomed.zoom <= 2);
  const back = C.zoomToward(zoomed, 0.01, 100, 540, 1920, 1080, 1080, 1920);
  assert.equal(back.zoom, 1);
  assert.ok(Math.abs(back.panX) < 1e-6 && Math.abs(back.panY) < 1e-6);
});

test('Ken Burns wide end is the whole image and the other end only zooms in', () => {
  for (const preset of KB) {
    const ends = C.wholeImageEndpoints(preset, 4, 4);
    const wide = ends.from.s <= ends.to.s ? ends.from : ends.to;
    const close = ends.from.s <= ends.to.s ? ends.to : ends.from;
    assert.equal(wide.s, 1);
    assert.equal(wide.x, 0);
    assert.equal(wide.y, 0);
    assert.ok(close.s >= 1);
  }
});

test('Alt+Backspace resets and does not change the playlist count; 0 is not reset', () => {
  const alt = { key: 'Backspace', altKey: true, ctrlKey: false, metaKey: false, shiftKey: false };
  const plain = { key: 'Backspace', altKey: false, ctrlKey: false, metaKey: false, shiftKey: false };
  assert.equal(C.isAltBackspaceReset(alt), true);
  assert.equal(C.willRemovePlaylistItem(alt), false);
  assert.equal(C.playlistCountAfterKey(5, alt), 5);
  assert.equal(C.willRemovePlaylistItem(plain), true);
  assert.equal(C.playlistCountAfterKey(5, plain), 4);
  assert.equal(C.isHueUpKey('0'), true);
  assert.equal(C.isResetPanKey('0'), false);
  assert.equal(C.isZoomInKey('+'), true);
  assert.equal(C.isZoomInKey('='), true);
  assert.equal(C.isZoomOutKey('-'), true);
  assert.equal(C.isResetPanKey('p'), true);
});

test('date-taken survives a JPEG and PNG rewrite and a loose HEIC scan', () => {
  const date = '2019:07:04 12:30:45';
  const jpeg = C.injectJpegExif(new Uint8Array([0xff, 0xd8, 0xff, 0xd9]), date);
  assert.equal(C.readJpegDate(jpeg), date);
  const sig = new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]);
  const iend = new Uint8Array([0, 0, 0, 0, 73, 69, 78, 68, 0xae, 0x42, 0x60, 0x82]);
  const pngIn = new Uint8Array(sig.length + iend.length);
  pngIn.set(sig, 0); pngIn.set(iend, sig.length);
  const png = C.injectPngDate(pngIn, date);
  assert.equal(C.readPngDate(png), date);
  const heic = new Uint8Array(32);
  const stamp = '2020:01:02 03:04:05';
  for (let i = 0; i < stamp.length; i++) heic[4 + i] = stamp.charCodeAt(i);
  const copied = C.copyDateTaken(heic, new Uint8Array([0xff, 0xd8, 0xff, 0xd9]));
  assert.equal(C.readJpegDate(copied), stamp);
});

test('cast hook crops stills on the fly and refuses cropped video on TV', () => {
  const still = C.castMediaHook({ type: 'image', path: 'G:\\a.jpg', crop: { x: 0, y: 0, w: 0.5, h: 0.5, aspect: '1:1' } });
  assert.equal(still.kind, 'still');
  assert.equal(still.cropOnTv, true);
  assert.ok(still.crop && still.crop.w === 0.5);
  const video = C.castMediaHook({ type: 'video', path: 'G:\\a.mp4', crop: { x: 0, y: 0, w: 0.5, h: 1, aspect: 'free' } });
  assert.equal(video.cropOnTv, false);
  assert.match(video.message, /can't be cropped on the TV/);
});

test('the slideshow wires Alt+Backspace before Backspace removal and keeps Hue + on 0', () => {
  const html = fs.readFileSync(new URL('./SlideShowPro.html', import.meta.url), 'utf8');
  const resetAt = html.indexOf('isAltBackspaceReset');
  const backAt = html.indexOf("if (e.key === 'Backspace')");
  assert.ok(resetAt !== -1 && backAt !== -1 && resetAt < backAt);
  assert.match(html, /id:'hueUp',\s+label:'Hue \+',\s+def:'0'/);
  assert.match(html, /k === '=' && keysEqual\(keyMap\.zoomIn, '\+'\)/);
  assert.match(html, /id:'zoomIn',\s+label:'Zoom In',\s+def:'\+'/);
  assert.match(html, /id:'zoomOut',\s+label:'Zoom Out',\s+def:'-'/);
  assert.match(html, /id:'panRst',\s+label:'Reset Pan\/Zoom',\s+def:'p'/);
});
