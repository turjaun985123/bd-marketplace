"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import PostCard from "@/components/PostCard";
import { EmptyState } from "@/components/ui";
import {
  listFollowedPosts,
  listRecentPosts,
  type PublicPostList,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The platform blog discovery page (Wave 59): the
 * newest live posts across every active store — the
 * marketplace reads sellers' blogs the way it reads
 * their products (what is public and live, newest
 * first, no paid placement). A signed-in reader can
 * narrow the list to the stores they follow (the
 * reading counterpart of the Wave 54 follows feed);
 * signed-out readers get the platform list and a
 * sign-in hint for the followed view.
 */
export default function MarketplaceBlogPage() {
  const { ready, token } = useAuth();
  const [scope, setScope] = useState<"all" | "following">("all");
  const [data, setData] = useState<PublicPostList | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      const result =
        scope === "following" && token
          ? await listFollowedPosts(token, { limit: 24 })
          : await listRecentPosts(24);
      setData(result);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : "ব্লগ লোড করা যায়নি।");
    }
  }, [scope, token]);

  useEffect(() => {
    if (!ready) return;
    let cancelled = false;
    reload().catch(() => {
      if (!cancelled) setError("ব্লগ লোড করা যায়নি।");
    });
    return () => {
      cancelled = true;
    };
  }, [ready, reload]);

  return (
    <main>
      <h1>ব্লগ — Blog</h1>
      <p>
        সব স্টোরের সর্বশেষ লেখা — the newest writing from every store on the
        platform.
      </p>
      <p>
        <button
          type="button"
          disabled={scope === "all"}
          onClick={() => setScope("all")}
        >
          সব স্টোর — All stores
        </button>{" "}
        <button
          type="button"
          disabled={scope === "following"}
          onClick={() => setScope("following")}
        >
          আমার ফলো করা স্টোর — Stores I follow
        </button>
      </p>
      {scope === "following" && ready && !token && (
        <p>
          ফলো করা স্টোরের পোস্ট দেখতে{" "}
          <Link href="/login">সাইন ইন করুন — Sign in</Link>।
        </p>
      )}
      {error && <p role="alert">{error}</p>}
      {data === null && !error ? (
        <p>লোড হচ্ছে…</p>
      ) : data && data.items.length === 0 ? (
        <EmptyState
          title={
            scope === "following"
              ? "ফলো করা স্টোর থেকে এখনো কোনো পোস্ট নেই"
              : "এখনো কোনো পোস্ট প্রকাশ হয়নি"
          }
        >
          <p>
            {scope === "following"
              ? "আপনার ফলো করা স্টোরগুলো ব্লগ পোস্ট প্রকাশ করলে এখানে দেখা যাবে।"
              : "স্টোরগুলো ব্লগ পোস্ট প্রকাশ করলে এখানে দেখা যাবে।"}
          </p>
        </EmptyState>
      ) : data ? (
        <div className="post-grid">
          {data.items.map((post) => (
            <PostCard key={post.id} post={post} showStore />
          ))}
        </div>
      ) : null}
    </main>
  );
}
