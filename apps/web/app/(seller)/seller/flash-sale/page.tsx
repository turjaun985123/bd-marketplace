"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import { Badge, Countdown, EmptyState } from "@/components/ui";
import { useAuth } from "@/hooks/useAuth";
import {
  getSellerFlashSale,
  type SellerFlashSale,
} from "@/lib/api";
import { formatBdt, formatDateTime } from "@/lib/format";

/**
 * The seller's seat on the platform flash sale (Wave 52):
 * when a sale is live, the seller sees exactly which of
 * their products it carries, the price buyers are actually
 * paying, and how much of each item's stock cap is already
 * sold — the same cap accounting the price resolution
 * itself uses, read from GET /v1/seller/flash-sale.
 *
 * The page is read-only by design: flash sales are a
 * platform event (admin-curated), and a seller's levers
 * stay their base price and their stock. What the seller
 * gets here is the truth about the event, live — including
 * the honest states: no sale running, or a sale running
 * that carries none of their products.
 */
export default function SellerFlashSalePage() {
  const { ready, token } = useAuth();
  const [sale, setSale] = useState<SellerFlashSale | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getSellerFlashSale(token)
      .then((data) => {
        if (cancelled) return;
        setSale(data);
        setLoaded(true);
      })
      .catch((err) => {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : String(err));
        setLoaded(true);
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
        <h1>ফ্ল্যাশ সেল — Flash sale</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>ফ্ল্যাশ সেল — Flash sale</h1>
      <p>
        প্ল্যাটফর্মের চলমান ফ্ল্যাশ সেলে আপনার পণ্য থাকলে এখানে দেখুন কোন
        দামে বিক্রি হচ্ছে আর ক্যাপের কতটা বিক্রি হয়েছে। সেল চলাকালীন
        ক্রেতা সেল মূল্যই দেয়, আর আপনার পে-আউটও সেই মূল্য থেকে হিসাব হয়
        — কমিশনসহ। (If the platform's live flash sale carries your
        products, here is the price buyers actually pay and how much of
        each cap is sold. During a sale the buyer pays the sale price,
        and your payout derives from that price, commission included.)
      </p>
      <p>
        <Link href="/seller">ড্যাশবোর্ডে ফিরুন — Back to the dashboard</Link>
      </p>
      {error && (
        <p role="alert" className="merch-error">
          {error}
        </p>
      )}
      {loaded && sale === null && !error && (
        <EmptyState title="এখন কোনো ফ্ল্যাশ সেল চলছে না — No flash sale is live">
          <p>
            নতুন সেল শুরু হলে আপনার পণ্য থাকলে এখানেই দেখা যাবে। (When a
            new sale starts, your products in it will appear here.)
          </p>
        </EmptyState>
      )}
      {sale && (
        <section>
          <h2>
            {sale.title} <Badge tone="flash">লাইভ — Live</Badge>
          </h2>
          <p>
            {formatDateTime(sale.starts_at)} → {formatDateTime(sale.ends_at)}{" "}
            · শেষ হতে বাকি — Ends in <Countdown endsAt={sale.ends_at} />
          </p>
          {sale.items.length === 0 ? (
            <EmptyState title="এই সেলে আপনার কোনো পণ্য নেই — None of your products are in this sale">
              <p>
                সেলটি চলছে, কিন্তু আপনার স্টোরের কোনো পণ্য এতে নেই।
                (The sale is running, but it carries none of your store's
                products.)
              </p>
            </EmptyState>
          ) : (
            <table className="merch-table">
              <thead>
                <tr>
                  <th>পণ্য — Product</th>
                  <th>স্বাভাবিক মূল্য — Base price</th>
                  <th>সেল মূল্য — Sale price</th>
                  <th>ছাড় — Off</th>
                  <th>বিক্রি / ক্যাপ — Sold / cap</th>
                </tr>
              </thead>
              <tbody>
                {sale.items.map((item) => {
                  const off =
                    item.base_price_poisha > 0
                      ? Math.floor(
                          ((item.base_price_poisha - item.sale_price_poisha) *
                            100) /
                            item.base_price_poisha
                        )
                      : 0;
                  return (
                    <tr key={item.product_id}>
                      <td>
                        <Link href={`/products/${item.product_id}`}>
                          {item.product_title}
                        </Link>{" "}
                        ({item.store_slug})
                      </td>
                      <td>{formatBdt(item.base_price_poisha)}</td>
                      <td>{formatBdt(item.sale_price_poisha)}</td>
                      <td>{off > 0 ? `-${off}%` : "—"}</td>
                      <td>
                        {item.sold_at_sale}
                        {item.stock_cap !== null
                          ? ` / ${item.stock_cap} (বাকি ${item.cap_remaining ?? 0} — left)`
                          : " (ক্যাপ নেই — uncapped)"}
                        {item.stock_cap !== null && item.stock_cap > 0 && (
                          <div
                            className="flash-progress"
                            role="progressbar"
                            aria-valuenow={item.sold_at_sale}
                            aria-valuemin={0}
                            aria-valuemax={item.stock_cap}
                          >
                            <div
                              className="flash-progress-bar"
                              style={{
                                width: `${Math.min(
                                  100,
                                  Math.round(
                                    (item.sold_at_sale * 100) / item.stock_cap
                                  )
                                )}%`,
                              }}
                            />
                          </div>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}
        </section>
      )}
    </main>
  );
}
