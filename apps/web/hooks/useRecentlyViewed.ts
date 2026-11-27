"use client";

import { useCallback, useEffect, useState } from "react";

const STORAGE_KEY = "recently-viewed";
/** The rail never shows more than this many products. */
export const RECENTLY_VIEWED_LIMIT = 12;

/**
 * One recently-viewed entry (Wave 53): the product id
 * plus the minimal card snapshot taken at view time, so
 * the rail renders instantly without a fetch per product.
 * Entries stored by Wave 24 (bare id strings) migrate on
 * read: they carry null snapshot fields and the rail
 * resolves those through the public product endpoint,
 * exactly as it did before.
 */
export interface RecentlyViewedEntry {
  id: string;
  title: string | null;
  store_slug: string | null;
  price_poisha: number | null;
  flash_price_poisha: number | null;
  image_url: string | null;
}

/** The snapshot the product page records on view. */
export interface RecentlyViewedSnapshot {
  id: string;
  title: string;
  store_slug: string;
  price_poisha: number;
  flash_price_poisha: number | null;
  image_url: string | null;
}

/**
 * Recently viewed (Wave 24, snapshot upgrade Wave 53) —
 * deliberately DEVICE-LOCAL. The history lives in this
 * browser's localStorage and is never sent to the server
 * as tracking data; the snapshot fields are display data
 * the visitor already saw on the product page. Clearing
 * the browser's site data (or the rail's own clear
 * button) clears the history — that is the privacy
 * model, stated plainly.
 */
function parseStored(raw: string | null): RecentlyViewedEntry[] {
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    const entries: RecentlyViewedEntry[] = [];
    for (const item of parsed) {
      if (typeof item === "string") {
        // Wave 24's bare id list.
        entries.push({
          id: item,
          title: null,
          store_slug: null,
          price_poisha: null,
          flash_price_poisha: null,
          image_url: null,
        });
      } else if (
        item !== null &&
        typeof item === "object" &&
        typeof (item as RecentlyViewedEntry).id === "string"
      ) {
        const entry = item as RecentlyViewedEntry;
        entries.push({
          id: entry.id,
          title: entry.title ?? null,
          store_slug: entry.store_slug ?? null,
          price_poisha: entry.price_poisha ?? null,
          flash_price_poisha: entry.flash_price_poisha ?? null,
          image_url: entry.image_url ?? null,
        });
      }
    }
    return entries.slice(0, RECENTLY_VIEWED_LIMIT);
  } catch {
    return [];
  }
}

function readStored(): RecentlyViewedEntry[] {
  try {
    return parseStored(localStorage.getItem(STORAGE_KEY));
  } catch {
    return [];
  }
}

/** Record a product view, most recent first, capped. */
export function recordRecentlyViewed(snapshot: RecentlyViewedSnapshot): void {
  try {
    const existing = readStored();
    const entry: RecentlyViewedEntry = { ...snapshot };
    const next = [
      entry,
      ...existing.filter((item) => item.id !== snapshot.id),
    ].slice(0, RECENTLY_VIEWED_LIMIT);
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Storage full or unavailable (private mode): the rail
    // simply stays as it was — viewing a product must never fail.
  }
}

/** Forget the whole history (the rail's clear button). */
export function clearRecentlyViewed(): void {
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    // Nothing to clear against; the rail still empties.
  }
}

/**
 * The stored entries, most recent first; empty until
 * mounted. `clear` empties both the storage and this
 * hook's state in one action.
 */
export function useRecentlyViewed(): {
  entries: RecentlyViewedEntry[];
  clear: () => void;
} {
  const [entries, setEntries] = useState<RecentlyViewedEntry[]>([]);

  useEffect(() => {
    setEntries(readStored());
  }, []);

  const clear = useCallback(() => {
    clearRecentlyViewed();
    setEntries([]);
  }, []);

  return { entries, clear };
}
