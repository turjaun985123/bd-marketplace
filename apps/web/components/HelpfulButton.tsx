"use client";

import { useEffect, useState } from "react";
import {
  getReviewHelpfulState,
  unvoteReviewHelpful,
  voteReviewHelpful,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The helpful-vote button (Wave 54 extension): shows the
 * review's helpful count and, for a signed-in non-author,
 * toggles the viewer's own vote. The viewer's state is
 * read from the API on mount (the review list itself is
 * anonymous data — it cannot carry per-viewer state), and
 * every toggle renders the SERVER's returned state. The
 * author sees the count without a button: the API
 * refuses self-votes, and the UI does not offer what it
 * would only refuse. Signed-out visitors see the count
 * as plain text.
 */
export default function HelpfulButton({
  reviewId,
  authorId,
  initialCount,
}: {
  reviewId: string;
  authorId: string;
  initialCount: number;
}) {
  const { token, user } = useAuth();
  const [helpful, setHelpful] = useState(false);
  const [count, setCount] = useState(initialCount);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getReviewHelpfulState(token, reviewId)
      .then((state) => {
        if (!cancelled) {
          setHelpful(state.helpful);
          setCount(state.helpful_count);
        }
      })
      .catch(() => {
        // The count from the list stands; the toggle
        // simply starts unpressed.
      });
    return () => {
      cancelled = true;
    };
  }, [token, reviewId]);

  if (!token || (user && user.id === authorId)) {
    return (
      <span className="helpful-count">
        {count > 0 ? `${count} জনের কাজে লেগেছে — helpful` : ""}
      </span>
    );
  }

  async function onToggle() {
    if (!token || busy) return;
    setBusy(true);
    try {
      const state = helpful
        ? await unvoteReviewHelpful(token, reviewId)
        : await voteReviewHelpful(token, reviewId);
      setHelpful(state.helpful);
      setCount(state.helpful_count);
    } catch {
      // A failed toggle changes nothing on screen; the
      // next mount re-reads the truth.
    } finally {
      setBusy(false);
    }
  }

  return (
    <span className="helpful-control">
      <button
        type="button"
        aria-pressed={helpful}
        disabled={busy}
        onClick={() => void onToggle()}
      >
        {helpful ? "✓ কাজে লেগেছে" : "কাজে লেগেছে — Helpful"}
        {count > 0 ? ` (${count})` : ""}
      </button>
    </span>
  );
}
