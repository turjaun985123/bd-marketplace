"use client";

import { useState } from "react";

/**
 * The share row (Wave 54): one button that uses the
 * device's native share sheet when the browser offers
 * one (`navigator.share` — phones), and otherwise copies
 * the page link to the clipboard and says so. Client-only
 * and honest: nothing is posted anywhere by the platform,
 * no share counts are tracked or shown — the button just
 * hands the visitor their own share tools.
 */
export default function ShareRow({
  title,
  path,
}: {
  title: string;
  /** The path to share; the origin is the visitor's own. */
  path: string;
}) {
  const [note, setNote] = useState<string | null>(null);

  async function onShare() {
    const url = `${window.location.origin}${path}`;
    setNote(null);
    // Read the optional share method through a widened
    // type: an `"share" in navigator` check narrows the
    // fall-through `navigator` to `never` in the DOM lib
    // (share is declared non-optional there), which then
    // rejects the clipboard access below at type-check.
    const nav = navigator as Navigator & {
      share?: (data: { title: string; url: string }) => Promise<void>;
    };
    if (typeof nav.share === "function") {
      try {
        await nav.share({ title, url });
        return;
      } catch {
        // A dismissed share sheet is not an error to
        // report; fall through to the copy path only when
        // sharing is genuinely unavailable.
        return;
      }
    }
    try {
      await navigator.clipboard.writeText(url);
      setNote("লিংক কপি হয়েছে ✓ — Link copied");
    } catch {
      setNote(url);
    }
  }

  return (
    <p className="share-row">
      <button type="button" onClick={() => void onShare()}>
        শেয়ার করুন — Share
      </button>
      {note && <span role="status"> {note}</span>}
    </p>
  );
}
