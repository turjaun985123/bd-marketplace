"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import { Badge, Button, EmptyState } from "@/components/ui";
import { useAuth } from "@/hooks/useAuth";
import {
  getAdminHomePreview,
  type HomeFeed,
} from "@/lib/api";
import {
  formatBdt,
  formatDateTime,
  isoToLocalInput,
  localInputToIso,
} from "@/lib/format";

/**
 * Scheduled-merchandising preview (Wave 52): the home feed
 * rendered as of a chosen instant, through the API's
 * home-preview endpoint — the SAME builder the public feed
 * uses, with the clock handed to it. An admin schedules a
 * banner window or a flash sale, picks the minute it goes
 * live here, and sees exactly what shoppers will see:
 * which banners are visible, which sale is live with which
 * prices, which collections appear.
 *
 * The page renders a structured summary (not the storefront
 * itself): the point is verification, and a summary is what
 * verifies — counts, titles, windows and prices, section by
 * section. "Now" is one click, for the live sanity check.
 */
export default function AdminHomePreviewPage() {
  const { token } = useAuth();
  const [at, setAt] = useState("");
  const [feed, setFeed] = useState<HomeFeed | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [previewedAt, setPreviewedAt] = useState<string | null>(null);

  const runPreview = useCallback(
    async (iso: string | undefined, label: string) => {
      if (!token) return;
      setBusy(true);
      setError(null);
      try {
        const data = await getAdminHomePreview(token, iso);
        setFeed(data);
        setPreviewedAt(label);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setBusy(false);
      }
    },
    [token]
  );

  // The page opens on the live feed: the baseline every
  // scheduled preview is compared against.
  useEffect(() => {
    if (token) void runPreview(undefined, "এখন — now");
  }, [token, runPreview]);

  function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    const iso = localInputToIso(at);
    if (!iso) {
      setError("একটি সময় বেছে নিন। (Pick a date and time to preview.)");
      return;
    }
    void runPreview(iso, formatDateTime(iso));
  }

  return (
    <AdminShell>
      <h1>হোম প্রিভিউ — Home preview</h1>
      <p>
        নির্দিষ্ট মুহূর্তে হোমপেজ দেখতে কেমন হবে — ব্যানার উইন্ডো ও
        ফ্ল্যাশ সেল যাচাই করার জন্য। (What the home page will look like
        at a chosen instant — for verifying banner windows and flash
        sales before they go live.)
      </p>
      {error && (
        <p role="alert" className="merch-error">
          {error}
        </p>
      )}
      <form onSubmit={onSubmit} className="merch-form">
        <div className="merch-form-row">
          <label>
            মুহূর্ত — Preview at
            <input
              type="datetime-local"
              value={at}
              onChange={(e) => setAt(e.target.value)}
            />
          </label>
        </div>
        <p>
          <Button type="submit" variant="primary" loading={busy}>
            প্রিভিউ দেখুন — Preview
          </Button>{" "}
          <Button
            variant="ghost"
            onClick={() => {
              setAt(isoToLocalInput(new Date().toISOString()));
              void runPreview(undefined, "এখন — now");
            }}
          >
            এখনকার অবস্থা — As of now
          </Button>
        </p>
      </form>

      {feed && (
        <section>
          <h2>
            প্রিভিউ: {previewedAt ?? "—"} — Preview
          </h2>

          <h3>ব্যানার — Banners ({feed.banners.length})</h3>
          {feed.banners.length === 0 ? (
            <EmptyState title="এই মুহূর্তে কোনো ব্যানার দেখাবে না — No banner visible at this instant" />
          ) : (
            <ul>
              {feed.banners.map((banner) => (
                <li key={banner.id}>
                  <strong>{banner.title}</strong>
                  {banner.subtitle ? ` — ${banner.subtitle}` : ""}
                  {banner.image_url ? " (ছবিসহ — with image)" : " (শুধু লেখা — text only)"}
                  {banner.link_url ? ` → ${banner.link_url}` : ""}
                </li>
              ))}
            </ul>
          )}

          <h3>ফ্ল্যাশ সেল — Flash sale</h3>
          {feed.flash_sale === null ? (
            <EmptyState title="এই মুহূর্তে কোনো সেল লাইভ নয় — No sale live at this instant" />
          ) : (
            <>
              <p>
                <strong>{feed.flash_sale.title}</strong>{" "}
                <Badge tone="flash">লাইভ — Live</Badge>{" "}
                {formatDateTime(feed.flash_sale.starts_at)} →{" "}
                {formatDateTime(feed.flash_sale.ends_at)} ·{" "}
                {feed.flash_sale.items.length}টি পণ্য — products
              </p>
              <ul>
                {feed.flash_sale.items.map((item) => (
                  <li key={item.id}>
                    {item.title}: {formatBdt(item.sale_price_poisha)}{" "}
                    <s>{formatBdt(item.price_poisha)}</s>
                    {item.stock_cap !== null
                      ? ` · ক্যাপ ${item.stock_cap}, বিক্রি ${item.sold_at_sale} — cap ${item.stock_cap}, sold ${item.sold_at_sale}`
                      : ""}
                  </li>
                ))}
              </ul>
            </>
          )}

          <h3>কালেকশন — Collections ({feed.collections.length})</h3>
          {feed.collections.length === 0 ? (
            <EmptyState title="কোনো কালেকশন সেকশন নেই — No collection sections" />
          ) : (
            <ul>
              {feed.collections.map((collection) => (
                <li key={collection.id}>
                  <strong>{collection.title}</strong> (/{collection.slug}) ·{" "}
                  {collection.products.length}টি পণ্য দেখাবে — products shown
                </li>
              ))}
            </ul>
          )}

          <h3>রেইল ও স্টোর — Rails & stores</h3>
          <ul>
            <li>ক্যাটাগরি টাইল — Category tiles: {feed.categories.length}</li>
            <li>নতুন এসেছে — New arrivals: {feed.new_arrivals.length}</li>
            <li>সেরা বিক্রিত — Best sellers: {feed.best_sellers.length}</li>
            <li>টপ রেটেড — Top rated: {feed.top_rated.length}</li>
            <li>স্পন্সরড — Promoted: {feed.promoted.length}</li>
            <li>
              যাচাইকৃত স্টোর — Featured stores: {feed.featured_stores.length}
            </li>
          </ul>
        </section>
      )}
    </AdminShell>
  );
}
