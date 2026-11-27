/**
 * Wave 38: PWA wiring check — dependency-free, run by CI
 * (and by `npm run check-pwa`) from apps/web. It does NOT
 * run a browser; it guards the load-bearing facts the
 * browser will rely on:
 *
 * 1. public/sw.js exists and still carries the NEVER-
 *    cache deny markers for the authenticated surfaces
 *    (/v1/auth/, /v1/cart/, /v1/orders). If someone
 *    "simplifies" the worker and drops the deny rules,
 *    this fails before a buyer's cart can be cached.
 * 2. app/manifest.ts exists (Next serves it as
 *    /manifest.webmanifest).
 * 3. The three manifest icons + apple-touch-icon exist,
 *    are real PNGs (signature bytes), and their IHDR
 *    dimensions match the sizes their names promise —
 *    a 512 file that is secretly 192 fails here, not on
 *    a user's home screen.
 *
 * Exits non-zero with a plain message on the first
 * failure. Regenerate icons with scripts/make-icons.mjs.
 */
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const webRoot = join(dirname(fileURLToPath(import.meta.url)), "..");

function fail(message) {
  console.error(`check-pwa: FAIL — ${message}`);
  process.exit(1);
}

/* 1. service worker + never-cache markers */
const swPath = join(webRoot, "public", "sw.js");
if (!existsSync(swPath)) fail("public/sw.js is missing");
const sw = readFileSync(swPath, "utf8");
for (const marker of ["/v1/auth/", "/v1/cart/", "/v1/orders"]) {
  if (!sw.includes(marker)) {
    fail(`public/sw.js lost its never-cache marker for ${marker}`);
  }
}
if (!sw.includes("NEVER_CACHE_PREFIXES")) {
  fail("public/sw.js lost the NEVER_CACHE_PREFIXES deny list");
}

/* 2. manifest */
if (!existsSync(join(webRoot, "app", "manifest.ts"))) {
  fail("app/manifest.ts is missing");
}

/* 3. icons: existence, PNG signature, IHDR dimensions */
const PNG_SIGNATURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
const icons = [
  { file: join(webRoot, "public", "icons", "icon-192.png"), size: 192 },
  { file: join(webRoot, "public", "icons", "icon-512.png"), size: 512 },
  { file: join(webRoot, "public", "icons", "icon-maskable-512.png"), size: 512 },
  { file: join(webRoot, "public", "apple-touch-icon.png"), size: 180 },
];
for (const icon of icons) {
  if (!existsSync(icon.file)) fail(`${icon.file} is missing`);
  const bytes = readFileSync(icon.file);
  if (bytes.length < 24) fail(`${icon.file} is too small to be a PNG`);
  for (let i = 0; i < 8; i++) {
    if (bytes[i] !== PNG_SIGNATURE[i]) fail(`${icon.file} is not a PNG`);
  }
  // IHDR width/height are big-endian uint32s at bytes 16..24.
  const width = bytes.readUInt32BE(16);
  const height = bytes.readUInt32BE(20);
  if (width !== icon.size || height !== icon.size) {
    fail(`${icon.file} is ${width}x${height}, expected ${icon.size}x${icon.size}`);
  }
}

console.log("check-pwa: OK — sw.js deny rules, manifest, and 4 icons verified");
