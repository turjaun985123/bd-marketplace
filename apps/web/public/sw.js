/**
 * BD Marketplace service worker (Wave 38) — hand-written,
 * no Workbox, no dependencies. Plain script in public/ so
 * it is served at /sw.js with the root scope.
 *
 * THE RULES (deliberately conservative — a marketplace
 * must never show one person's data to another, and never
 * pretend a mutation happened):
 *
 * 1. Only same-origin GET requests are handled at all.
 *    Everything else — every POST/PUT/PATCH/DELETE, every
 *    cross-origin call — passes through untouched. (In
 *    the deployed shape the API is same-origin: the web
 *    bundle bakes NEXT_PUBLIC_API_BASE_URL="" and a proxy
 *    routes /v1 to the API — see .github/workflows/ci.yml.)
 *
 * 2. NEVER CACHE (network only, nothing stored): any path
 *    under the NEVER_CACHE_PREFIXES below — auth, cart,
 *    orders, seller, admin, payments, loyalty, wishlist,
 *    conversations, API keys. These are authenticated or
 *    mutating surfaces; a cached copy could be stale,
 *    personal, or both. The check script
 *    (scripts/check-pwa.mjs) fails CI if these markers
 *    disappear from this file.
 *
 * 3. CACHE-FIRST (immutable build assets): /_next/static/*
 *    (content-hashed by Next), /icons/*, and
 *    /manifest.webmanifest. Wave 51 adds /media/* — the
 *    API serves product images there with
 *    `Cache-Control: immutable`, and a media key's bytes
 *    never change (a replaced image gets a new key), so
 *    cache-first is exact, not approximate.
 *
 * 4. NETWORK-FIRST, CACHE FALLBACK (public catalogue):
 *    navigations, and GETs for the public read paths in
 *    PUBLIC_API_PREFIXES (/v1/products incl. detail and
 *    reviews, /v1/search + suggest, /v1/stores incl. a
 *    store's profile and products). Fresh when online;
 *    the last-seen copy when the network drops — on flaky
 *    BD connections a slightly stale catalogue beats a
 *    dead page. These endpoints are public (no token
 *    needed) and carry no per-user data.
 *
 * 5. OFFLINE FALLBACK: a navigation that fails with no
 *    cached copy gets the precached /offline page.
 *
 * Caches are versioned (w51-v1): bump VERSION on any
 * rule change — activate deletes every older cache.
 */
"use strict";

var VERSION = "w51-v1";
var STATIC_CACHE = "bdm-static-" + VERSION;
var PAGE_CACHE = "bdm-pages-" + VERSION;
var API_CACHE = "bdm-api-" + VERSION;

var APP_SHELL = ["/", "/offline", "/manifest.webmanifest", "/icons/icon-192.png", "/icons/icon-512.png"];

// Rule 2 — NEVER cache these path prefixes (same-origin).
// check-pwa.mjs asserts the auth/cart/orders markers.
var NEVER_CACHE_PREFIXES = [
  "/v1/auth/",
  "/v1/cart/",
  "/v1/orders",
  "/v1/seller/",
  "/v1/admin/",
  "/v1/payments/",
  "/v1/loyalty",
  "/v1/wishlist",
  "/v1/conversations",
  "/v1/api-keys",
  // Wave 55: the address book is personal data — never cached.
  "/v1/addresses",
  // Wave 55: buyer-scoped reads (export, buy-again rail)
  // are personal data too.
  "/v1/buyer/",
];

// Rule 4 — public, per-user-data-free API reads.
var PUBLIC_API_PREFIXES = ["/v1/products", "/v1/search", "/v1/stores"];

// Rule 3 — immutable static assets (Wave 51: /media/ joins
// them — product images are content-addressed by key and
// served immutable by the API).
var STATIC_PREFIXES = ["/_next/static/", "/icons/", "/media/"];
var STATIC_EXACT = ["/manifest.webmanifest"];

function startsWithAny(path, prefixes) {
  for (var i = 0; i < prefixes.length; i++) {
    if (path.indexOf(prefixes[i]) === 0) return true;
  }
  return false;
}

function putInCache(cacheName, request, response) {
  var copy = response.clone();
  caches
    .open(cacheName)
    .then(function (cache) {
      return cache.put(request, copy);
    })
    .catch(function () {
      /* a failed store must never fail the request */
    });
}

self.addEventListener("install", function (event) {
  event.waitUntil(
    caches
      .open(STATIC_CACHE)
      .then(function (cache) {
        return cache.addAll(APP_SHELL);
      })
      .then(function () {
        return caches.open(PAGE_CACHE).then(function (cache) {
          return cache.add("/offline");
        });
      })
  );
});

self.addEventListener("activate", function (event) {
  event.waitUntil(
    caches
      .keys()
      .then(function (keys) {
        return Promise.all(
          keys.map(function (key) {
            if (key !== STATIC_CACHE && key !== PAGE_CACHE && key !== API_CACHE) {
              return caches.delete(key);
            }
            return Promise.resolve(false);
          })
        );
      })
      .then(function () {
        return self.clients.claim();
      })
  );
});

// The page can ask a waiting worker to take over now
// (update flow: registration.waiting.postMessage(...)).
self.addEventListener("message", function (event) {
  if (event.data && event.data.type === "SKIP_WAITING") {
    self.skipWaiting();
  }
});

self.addEventListener("fetch", function (event) {
  var request = event.request;

  // Rule 1: only same-origin GETs are handled at all.
  if (request.method !== "GET") return;
  var url = new URL(request.url);
  if (url.origin !== self.location.origin) return;
  var path = url.pathname;

  // Rule 2: never cache — do not even respondWith; the
  // browser talks to the network exactly as if no worker
  // existed, and nothing is stored.
  if (startsWithAny(path, NEVER_CACHE_PREFIXES)) return;

  // Rule 3: cache-first for immutable static assets.
  if (startsWithAny(path, STATIC_PREFIXES) || STATIC_EXACT.indexOf(path) !== -1) {
    event.respondWith(
      caches.match(request).then(function (cached) {
        if (cached) return cached;
        return fetch(request).then(function (response) {
          if (response.ok) putInCache(STATIC_CACHE, request, response);
          return response;
        });
      })
    );
    return;
  }

  // Rule 5 + 4: navigations are network-first; a failed
  // navigation falls back to the cached page, then to
  // the precached /offline page.
  if (request.mode === "navigate") {
    event.respondWith(
      fetch(request)
        .then(function (response) {
          if (response.ok) putInCache(PAGE_CACHE, request, response);
          return response;
        })
        .catch(function () {
          return caches.match(request).then(function (cached) {
            if (cached) return cached;
            return caches.match("/offline");
          });
        })
    );
    return;
  }

  // Rule 4: public catalogue API reads, network-first.
  if (startsWithAny(path, PUBLIC_API_PREFIXES)) {
    event.respondWith(
      fetch(request)
        .then(function (response) {
          if (response.ok) putInCache(API_CACHE, request, response);
          return response;
        })
        .catch(function () {
          return caches.match(request).then(function (cached) {
            if (cached) return cached;
            throw new Error("offline and not cached: " + path);
          });
        })
    );
    return;
  }

  // Anything else (other same-origin GETs — RSC payloads,
  // images, misc): pass through untouched.
});
