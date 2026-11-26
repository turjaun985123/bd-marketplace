"use client";

import { useEffect, useState } from "react";

const STORAGE_KEY = "recently-viewed";
/** The rail never shows more than this many products. */
export const RECENTLY_VIEWED_LIMIT = 12;

/**
 * Recently viewed (Wave 24) — deliberately DEVICE-LOCAL.
 * The viewed product ids live in this browser's localStorage
 * and are never sent to the server as tracking data; the
 * home rail resolves them through the ordinary public
 * product endpoint. Clearing the browser's site data clears
 * the history — that is the privacy model, stated plainly.
 */
export function recordRecentlyViewed(productId: string): void {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const existing: string[] = raw ? (JSON.parse(raw) as string[]) : [];
    const next = [
      productId,
      ...existing.filter((id) => id !== productId),
    ].slice(0, RECENTLY_VIEWED_LIMIT);
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Storage full or unavailable (private mode): the rail
    // simply stays as it was — viewing a product must never fail.
  }
}

/** The stored ids, most recent first; empty until mounted. */
export function useRecentlyViewed(): string[] {
  const [ids, setIds] = useState<string[]>([]);

  useEffect(() => {
    try {
      const raw = localStorage.getItem(STORAGE_KEY);
      if (raw) {
        const parsed = JSON.parse(raw) as string[];
        if (Array.isArray(parsed)) {
          setIds(parsed.slice(0, RECENTLY_VIEWED_LIMIT));
        }
      }
    } catch {
      setIds([]);
    }
  }, []);

  return ids;
}
