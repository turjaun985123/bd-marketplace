"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import FollowButton from "@/components/FollowButton";
import ProductRail from "@/components/ProductRail";
import SuggestedStores from "@/components/SuggestedStores";
import { EmptyState } from "@/components/ui";
import FollowsFeedRail from "@/components/FollowsFeedRail";
import {
  listFollows,
  setFollowNotify,
  type FollowedStore,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The following page (Wave 54): every store the signed-in
 * buyer follows, most recently followed first, each with
 * its follower count and a rail of its newest products —
 * the buyer's own shelf of shops, kept on the account so
 * it follows them across devices. The follow button on
 * each card renders the server's returned state, like
 * everywhere; a card unfollowed here stays visible (now
 * offering follow again) until the page is revisited. A
 * suspended store drops off the list server-side; the
 * follow itself survives, so the store returns if it is
 * reinstated.
 */
export default function FollowingPage() {
  const { ready, token } = useAuth();
  const [stores, setStores] = useState<FollowedStore[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listFollows(token);
      setStores(list.items);
      setError(null);
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "ফলো তালিকা লোড করা যায়নি।"
      );
    }
  }, [token]);

  useEffect(() => {
    if (!ready || !token) return;
    let cancelled = false;
    reload().catch(() => {
      if (!cancelled) setError("ফলো তালিকা লোড করা যায়নি।");
    });
    return () => {
      cancelled = true;
    };
  }, [ready, token, reload]);

  if (!ready) {
    return (
      <main>
        <p>লোড হচ্ছে…</p>
      </main>
    );
  }

  if (!token) {
    return (
      <main>
        <h1>ফলো করা স্টোর — Following</h1>
        <p>
          ফলো করা স্টোর দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>ফলো করা স্টোর — Stores you follow</h1>
      <p>
        ফলো করা স্টোর নতুন পণ্য আনলে আপনাকে জানানো হবে — followed stores
        notify you when they publish a new product.
      </p>
      {error && <p role="alert">{error}</p>}
      {stores === null ? (
        <p>লোড হচ্ছে…</p>
      ) : stores.length === 0 ? (
        <EmptyState title="আপনি এখনো কোনো স্টোর ফলো করেননি — You follow no stores yet">
          <p>
            পছন্দের স্টোরের পেজে গিয়ে “ফলো করুন” চাপুন — নতুন পণ্য এলে
            এখানেই সবার আগে দেখতে পাবেন।
          </p>
          <p>
            <Link href="/browse">পণ্য দেখুন — Browse products</Link>
          </p>
        </EmptyState>
      ) : (
        stores.map((store) => (
          <section key={store.id} className="card following-store">
            <h2>
              <Link href={`/${store.slug}`}>{store.name}</Link>
            </h2>
            <p>
              {store.follower_count.toLocaleString("en-IN")} জন ফলোয়ার ·{" "}
              {store.product_count.toLocaleString("en-IN")}টি পণ্য · ফলো
              করেছেন{" "}
              {new Date(store.followed_at).toLocaleDateString("en-GB")}
            </p>
            <p>
              <FollowButton
                slug={store.slug}
                storeOwnerId={store.owner_user_id}
                initialFollowing
                initialCount={store.follower_count}
              />{" "}
              <Link href={`/${store.slug}/shop`}>
                স্টোরের সব পণ্য — Shop this store
              </Link>
            </p>
            <NotifyToggle store={store} token={token} onChanged={reload} />
            <ProductRail
              title="সর্বশেষ পণ্য — Newest products"
              products={store.newest_products}
            />
          </section>
        ))
      )}
      {/* The newest products across these stores — the
          same rail the home page carries. */}
      <FollowsFeedRail />
      <SuggestedStores />
    </main>
  );
}

/**
 * One follow's notice toggle (Wave 54 extension): muting
 * keeps the follow — the shelf and the feed — and stops
 * only the new-product notices. The button renders the
 * server's returned flag, and a successful toggle
 * reloads the list so the card and the state agree.
 */
function NotifyToggle({
  store,
  token,
  onChanged,
}: {
  store: FollowedStore;
  token: string;
  onChanged: () => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const on = store.notify_new_products;

  async function onToggle() {
    setBusy(true);
    try {
      await setFollowNotify(token, store.slug, !on);
      await onChanged();
    } catch {
      // The list still shows the server's last word.
    } finally {
      setBusy(false);
    }
  }

  return (
    <p>
      <button
        type="button"
        disabled={busy}
        aria-pressed={!on}
        onClick={() => void onToggle()}
      >
        {on
          ? "নতুন পণ্যের নোটিফিকেশন: চালু — Notices on (tap to mute)"
          : "নতুন পণ্যের নোটিফিকেশন: বন্ধ — Notices muted (tap to unmute)"}
      </button>
    </p>
  );
}
