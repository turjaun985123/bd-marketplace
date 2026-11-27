"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { getSellerFollowerStats, type FollowerStats } from "@/lib/api";

/**
 * The seller dashboard's follower card (Wave 54
 * extension): the store's follow total with the last
 * week's gain, linking to the full followers page with
 * its 30-day chart. Renders nothing while the store has
 * no followers — a dashboard card announcing zero is
 * noise, and a seller without a store (the stats read
 * 404s) gets nothing either.
 */
export default function SellerFollowersCard({ token }: { token: string }) {
  const [stats, setStats] = useState<FollowerStats | null>(null);

  useEffect(() => {
    let cancelled = false;
    getSellerFollowerStats(token)
      .then((value) => {
        if (!cancelled) setStats(value);
      })
      .catch(() => {
        if (!cancelled) setStats(null);
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  if (stats === null || stats.total === 0) return null;

  return (
    <section>
      <div className="card">
        <h2>ফলোয়ার — Followers</h2>
        <p>
          আপনার স্টোরের ফলোয়ার <strong>{stats.total}</strong> জন
          {stats.last_7_days > 0 &&
            ` · গত ৭ দিনে নতুন ${stats.last_7_days} জন`}
          {" · "}
          গত ৩০ দিনে {stats.last_30_days} জন।
        </p>
        <p>
          <Link href="/seller/followers">
            ফলোয়ারদের তালিকা ও চার্ট — Followers &amp; chart
          </Link>
        </p>
      </div>
    </section>
  );
}
