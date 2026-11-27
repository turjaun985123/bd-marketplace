"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import { Countdown } from "./ui";
import { getHomeFeed, type HomeFlashSale } from "@/lib/api";

/**
 * The site-wide flash sale strip (Wave 52): while a flash
 * sale is live, every marketplace page carries the campaign
 * bar — the sale's name, its live countdown, and the way to
 * the sale section on the home page. When no sale is live
 * (or the feed cannot be read) the strip renders nothing:
 * it is a campaign surface, not chrome, and an absent sale
 * must never leave an empty bar behind.
 *
 * The data is the home feed's flash block — the same
 * cap-checked resolution the home page renders — read once
 * per page mount.
 */
export default function FlashStrip() {
  const [sale, setSale] = useState<HomeFlashSale | null>(null);

  useEffect(() => {
    let cancelled = false;
    getHomeFeed()
      .then((feed) => {
        if (!cancelled) setSale(feed.flash_sale);
      })
      .catch(() => {
        if (!cancelled) setSale(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (sale === null || sale.items.length === 0) return null;

  return (
    <div className="flash-strip" role="region" aria-label="ফ্ল্যাশ সেল — Flash sale">
      <span>⚡ {sale.title}</span>
      <span>
        শেষ হতে বাকি — Ends in <Countdown endsAt={sale.ends_at} />
      </span>
      <Link href="/">সেলের পণ্য দেখুন — Shop the sale</Link>
    </div>
  );
}
