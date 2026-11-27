"use client";

import { useState } from "react";
import Link from "next/link";
import { followStore, unfollowStore } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The follow button (Wave 54): follows / unfollows one
 * store through the API and renders the SERVER's returned
 * state (following flag + follower count) — never an
 * optimistic guess, so the count on the button is the
 * count the storefront advertises. Signed-out visitors
 * get a login link instead (following is account data).
 * The store's owner never sees the button: the API
 * refuses a self-follow, and the UI does not offer what
 * it would only refuse.
 */
export default function FollowButton({
  slug,
  storeOwnerId,
  initialFollowing,
  initialCount,
}: {
  slug: string;
  storeOwnerId: string;
  initialFollowing: boolean;
  initialCount: number;
}) {
  const { token, user } = useAuth();
  const [following, setFollowing] = useState(initialFollowing);
  const [count, setCount] = useState(initialCount);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The owner's own storefront: no button (see the doc).
  if (user && user.id === storeOwnerId) return null;

  if (!token) {
    return (
      <Link className="btn btn--ghost follow-button" href="/login">
        ফলো করুন — Follow
      </Link>
    );
  }

  async function onToggle() {
    if (!token || busy) return;
    setBusy(true);
    setError(null);
    try {
      const state = following
        ? await unfollowStore(token, slug)
        : await followStore(token, slug);
      setFollowing(state.following);
      setCount(state.follower_count);
    } catch (err) {
      setError(err instanceof Error ? err.message : "ফলো করা যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  return (
    <span className="follow-control">
      <button
        type="button"
        className={
          following ? "btn follow-button follow-button--on" : "btn follow-button"
        }
        aria-pressed={following}
        disabled={busy}
        onClick={() => void onToggle()}
      >
        {following ? "✓ ফলো করেছেন" : "+ ফলো করুন — Follow"} ·{" "}
        {count.toLocaleString("en-IN")}
      </button>
      {error && (
        <span role="alert" className="follow-error">
          {" "}
          {error}
        </span>
      )}
    </span>
  );
}
