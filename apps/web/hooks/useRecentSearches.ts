"use client";

import { useCallback, useEffect, useState } from "react";

const STORAGE_KEY = "recent-searches";
/** The dropdown never lists more than this many recent searches. */
export const RECENT_SEARCHES_LIMIT = 8;

/**
 * Recent searches (Wave 53) — the shopper's own submitted
 * query texts, DEVICE-LOCAL like the recently-viewed
 * history: localStorage only, never sent to the server
 * (the server's analytics log is aggregate and
 * identifier-free by design; this list is the visitor's
 * own convenience on their own device). Clearing site
 * data, or the dropdown's clear button, forgets them.
 */
export function recordRecentSearch(query: string): void {
  const trimmed = query.trim();
  if (!trimmed) return;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const existing: string[] = raw ? (JSON.parse(raw) as string[]) : [];
    const next = [
      trimmed,
      ...existing.filter((entry) => entry !== trimmed),
    ].slice(0, RECENT_SEARCHES_LIMIT);
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Storage unavailable: the search still runs.
  }
}

/** Forget the whole recent-search list. */
export function clearRecentSearches(): void {
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    // Nothing to clear against; the dropdown still empties.
  }
}

/** The stored query texts, most recent first; empty until mounted. */
export function useRecentSearches(): {
  searches: string[];
  clear: () => void;
} {
  const [searches, setSearches] = useState<string[]>([]);

  useEffect(() => {
    try {
      const raw = localStorage.getItem(STORAGE_KEY);
      if (raw) {
        const parsed = JSON.parse(raw) as unknown;
        if (Array.isArray(parsed)) {
          setSearches(
            parsed
              .filter((entry): entry is string => typeof entry === "string")
              .slice(0, RECENT_SEARCHES_LIMIT)
          );
        }
      }
    } catch {
      setSearches([]);
    }
  }, []);

  const clear = useCallback(() => {
    clearRecentSearches();
    setSearches([]);
  }, []);

  return { searches, clear };
}
