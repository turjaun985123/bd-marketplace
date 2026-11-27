/**
 * Wave 38: PWA icon generator — dependency-free on purpose.
 *
 * Draws the app icons pixel-by-pixel and encodes real PNG
 * files using only Node's built-in `zlib` (the PNG IDAT
 * stream is just deflated scanlines). No image library, no
 * network fetch, no design-tool export: anyone can
 * regenerate the exact icons with
 *
 *   node scripts/make-icons.mjs
 *
 * The mark is deliberately abstract: a white shopping-bag
 * outline (rounded-rect body + semicircular handle) on the
 * brand green from app/globals.css (#1b5e20 — the dark
 * green of the verification-badge palette, the app's trust
 * colour). No text: at 48px a wordmark is mush.
 *
 * Variants:
 * - icon-192 / icon-512 (purpose "any"): rounded-square
 *   tile, transparent corners, motif at 64% of the canvas.
 * - icon-maskable-512 (purpose "maskable"): full-bleed
 *   square (the OS applies its own mask), motif shrunk to
 *   the centre ~54% so it stays inside the 80% safe-zone
 *   circle whatever mask shape the launcher uses.
 * - apple-touch-icon (180): full-bleed square — iOS rounds
 *   the corners itself, so pre-rounded corners would
 *   double-round.
 */
import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const BRAND_GREEN = [0x1b, 0x5e, 0x20]; // #1b5e20 (globals.css)
const WHITE = [0xff, 0xff, 0xff];

const webRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const iconsDir = join(webRoot, "public", "icons");
const publicDir = join(webRoot, "public");

/* ---------- PNG encoding (RGBA, filter 0 scanlines) ---------- */

const CRC_TABLE = (() => {
  const table = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c;
  }
  return table;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([len, body, crc]);
}

function encodePng(rgba, size) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // colour type 6 = RGBA
  // bytes 10-12 stay 0: deflate, adaptive filters, no interlace
  const stride = size * 4 + 1;
  const raw = Buffer.alloc(stride * size);
  for (let y = 0; y < size; y++) {
    raw[y * stride] = 0; // filter: none
    rgba.copy(raw, y * stride + 1, y * size * 4, (y + 1) * size * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/* ---------- the mark, as signed-distance math ---------- */

/** Signed distance to a rounded box centred at (cx, cy). */
function sdRoundBox(x, y, cx, cy, halfW, halfH, r) {
  const qx = Math.abs(x - cx) - (halfW - r);
  const qy = Math.abs(y - cy) - (halfH - r);
  const ax = Math.max(qx, 0);
  const ay = Math.max(qy, 0);
  return Math.hypot(ax, ay) + Math.min(Math.max(qx, qy), 0) - r;
}

/**
 * Is unit-space point (x, y) part of the white bag motif?
 * The motif is drawn inside a square box of side `box`
 * centred on the canvas; inside the box, local coords are
 * bag-local: body is a rounded-rect outline, handle is the
 * upper half of a circle whose diameter line is the body's
 * top edge, so the handle ends land exactly on the body.
 */
function inMotif(x, y, box) {
  const half = box / 2;
  const lx = (x - (0.5 - half)) / box; // 0..1 across the box
  const ly = (y - (0.5 - half)) / box;
  if (lx < 0 || lx > 1 || ly < 0 || ly > 1) return false;
  const stroke = 0.055;
  // Bag body: rounded-rect outline.
  const body = sdRoundBox(lx, ly, 0.5, 0.6, 0.3, 0.26, 0.05);
  if (Math.abs(body) <= stroke / 2) return true;
  // Handle: upper semicircle, centre on the body's top edge.
  const d = Math.abs(Math.hypot(lx - 0.5, ly - 0.34) - 0.15);
  if (d <= stroke / 2 && ly <= 0.34 + stroke / 2) return true;
  return false;
}

/** Is unit-space point (x, y) inside the background tile? */
function inTile(x, y, cornerRadius) {
  if (cornerRadius <= 0) return true; // full-bleed square
  return sdRoundBox(x, y, 0.5, 0.5, 0.5, 0.5, cornerRadius) <= 0;
}

const SAMPLES = 3; // 3x3 supersampling per pixel for smooth edges

function render(size, { cornerRadius, motifBox }) {
  const rgba = Buffer.alloc(size * size * 4);
  for (let py = 0; py < size; py++) {
    for (let px = 0; px < size; px++) {
      let r = 0, g = 0, b = 0, a = 0;
      for (let sy = 0; sy < SAMPLES; sy++) {
        for (let sx = 0; sx < SAMPLES; sx++) {
          const x = (px + (sx + 0.5) / SAMPLES) / size;
          const y = (py + (sy + 0.5) / SAMPLES) / size;
          if (!inTile(x, y, cornerRadius)) continue;
          const c = inMotif(x, y, motifBox) ? WHITE : BRAND_GREEN;
          r += c[0]; g += c[1]; b += c[2]; a += 255;
        }
      }
      const n = SAMPLES * SAMPLES;
      const i = (py * size + px) * 4;
      rgba[i] = Math.round(r / n);
      rgba[i + 1] = Math.round(g / n);
      rgba[i + 2] = Math.round(b / n);
      rgba[i + 3] = Math.round(a / n);
    }
  }
  return encodePng(rgba, size);
}

/* ---------- outputs ---------- */

mkdirSync(iconsDir, { recursive: true });

const outputs = [
  // purpose "any": rounded tile, transparent corners.
  { file: join(iconsDir, "icon-192.png"), size: 192, cornerRadius: 0.2, motifBox: 0.64 },
  { file: join(iconsDir, "icon-512.png"), size: 512, cornerRadius: 0.2, motifBox: 0.64 },
  // purpose "maskable": full bleed; motif inside the safe zone.
  { file: join(iconsDir, "icon-maskable-512.png"), size: 512, cornerRadius: 0, motifBox: 0.54 },
  // iOS home screen: full bleed, iOS masks it itself.
  { file: join(publicDir, "apple-touch-icon.png"), size: 180, cornerRadius: 0, motifBox: 0.64 },
];

for (const out of outputs) {
  writeFileSync(out.file, render(out.size, out));
  console.log(`wrote ${out.file} (${out.size}x${out.size})`);
}
