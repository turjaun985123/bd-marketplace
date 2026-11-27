"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  getSellerFollowerStats,
  listSellerFollowers,
  type FollowerStats,
  type StoreFollower,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The seller's followers page (Wave 54): who follows this
 * store, newest first, with the store's total on top.
 * Follower identities stay masked exactly as reviews mask
 * buyers (name when the buyer has one, else the masked
 * phone) — a follow is a subscription, not a contact
 * list, and the seller gains no new way to reach these
 * buyers beyond the platform's own messaging. New
 * followers learn about new products through the
 * platform's notification outbox, never a seller export.
 */
export default function SellerFollowersPage() {
  const { ready, token } = useAuth();
  const [items, setItems] = useState<StoreFollower[] | null>(null);
  const [stats, setStats] = useState<FollowerStats | null>(null);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [message, setMessage] = useState<string | null>(null);
  const LIMIT = 20;

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listSellerFollowers(token, {
        page,
        limit: LIMIT,
      });
      setItems(list.items);
      setTotal(list.total);
      setMessage(null);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "ফলোয়ার লোড করা যায়নি।"
      );
    }
  }, [token, page]);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    load().catch(() => {
      if (!cancelled) setMessage("ফলোয়ার লোড করা যায়নি।");
    });
    return () => {
      cancelled = true;
    };
  }, [token, load]);

  // The growth stats load once alongside the list (they
  // are store-level, not page-level).
  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getSellerFollowerStats(token)
      .then((data) => {
        if (!cancelled) setStats(data);
      })
      .catch(() => {
        if (!cancelled) setStats(null);
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

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
        <h1>ফলোয়ার — Followers</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  const pages = Math.max(1, Math.ceil(total / LIMIT));

  return (
    <main>
      <h1>ফলোয়ার — Followers</h1>
      <p>
        <Link href="/seller">← ড্যাশবোর্ডে ফিরুন — Dashboard</Link>
      </p>
      <p>
        মোট {total.toLocaleString("en-IN")} জন আপনার স্টোর ফলো করছেন —
        নতুন পণ্য প্রকাশ করলেই তাঁরা নোটিফিকেশন পাবেন।
      </p>
      {stats && (
        <section className="card follower-stats">
          <h2>বৃদ্ধি — Growth</h2>
          <p>
            গত ৭ দিনে +{stats.last_7_days} · গত ৩০ দিনে +
            {stats.last_30_days} — new followers in the last 7 / 30 days.
          </p>
          <div
            className="follower-chart"
            role="img"
            aria-label="গত ৩০ দিনের প্রতিদিনের নতুন ফলোয়ার"
          >
            {stats.daily.map((day) => {
              const max = Math.max(
                1,
                ...stats.daily.map((d) => d.new_followers)
              );
              return (
                <span
                  key={day.date}
                  className="follower-chart-bar"
                  style={{ height: `${(day.new_followers / max) * 100}%` }}
                  title={`${day.date}: ${day.new_followers}`}
                />
              );
            })}
          </div>
          <p className="follower-chart-note">
            প্রতিটি বার একটি দিন — ওপরে মাউস রাখলে তারিখ ও সংখ্যা দেখা
            যাবে। (Each bar is a day; hover for its date and count.)
          </p>
        </section>
      )}
      {message && <p role="alert">{message}</p>}
      {items === null ? (
        <p>লোড হচ্ছে…</p>
      ) : items.length === 0 ? (
        <p>
          এখনো কেউ ফলো করেননি — স্টোরফ্রন্টের লিংকটি শেয়ার করুন, ক্রেতারা
          ফলো করলেই এখানে দেখা যাবে।
        </p>
      ) : (
        <ul>
          {items.map((follower) => (
            <li key={follower.user_id}>
              {follower.display} — ফলো করেছেন{" "}
              {new Date(follower.followed_at).toLocaleDateString("en-GB")}
              {!follower.notify_new_products &&
                " · নোটিফিকেশন বন্ধ রেখেছেন — notices muted"}
            </li>
          ))}
        </ul>
      )}
      {pages > 1 && (
        <p>
          <button
            type="button"
            disabled={page <= 1}
            onClick={() => setPage(page - 1)}
          >
            ← আগের
          </button>{" "}
          পৃষ্ঠা {page} / {pages}{" "}
          <button
            type="button"
            disabled={page >= pages}
            onClick={() => setPage(page + 1)}
          >
            পরের →
          </button>
        </p>
      )}
    </main>
  );
}
