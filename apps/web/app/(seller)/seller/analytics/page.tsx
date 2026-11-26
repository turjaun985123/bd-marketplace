"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import DailyBars from "@/components/DailyBars";
import PriceTag from "@/components/PriceTag";
import {
  downloadSellerAnalyticsCsv,
  getSellerAnalytics,
  type SellerAnalytics,
} from "@/lib/api";
import { formatBdt } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

const DAY_OPTIONS = [7, 30, 90];

const STATUS_LABELS: Record<string, string> = {
  placed: "অর্ডার পড়েছে",
  confirmed: "নিশ্চিত",
  shipped: "পাঠানো হয়েছে",
  delivered: "ডেলিভারি হয়েছে",
  cancelled: "বাতিল",
  returned: "ফেরত এসেছে",
};

/**
 * Seller analytics (Wave 27): the seller's own numbers from
 * GET /v1/seller/analytics over the last 7/30/90 Asia/Dhaka
 * days. The definitions live on the API and in the README;
 * this page displays them and repeats each one in small
 * print under its block: goods exclude cancelled sub-orders,
 * earnings count DELIVERED sub-orders' payout snapshots
 * only, and the daily series is zero-filled so quiet days
 * still show. The CSV button downloads the same daily
 * series (its `orders` column counts sub-orders).
 */
export default function SellerAnalyticsPage() {
  const { ready, token } = useAuth();
  const [days, setDays] = useState(30);
  const [analytics, setAnalytics] = useState<SellerAnalytics | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [csvBusy, setCsvBusy] = useState(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      setAnalytics(await getSellerAnalytics(token, days));
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "অ্যানালিটিক্স লোড করা যায়নি।"
      );
    }
  }, [token, days]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onDownloadCsv() {
    if (!token) return;
    setCsvBusy(true);
    setMessage(null);
    try {
      const blob = await downloadSellerAnalyticsCsv(token, days);
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = `seller-analytics-${days}d.csv`;
      anchor.click();
      URL.revokeObjectURL(url);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "CSV ডাউনলোড করা যায়নি।");
    } finally {
      setCsvBusy(false);
    }
  }

  if (ready && !token) {
    return (
      <main>
        <h1>অ্যানালিটিক্স — Analytics</h1>
        <p>
          অ্যানালিটিক্স দেখতে আগে <Link href="/login">লগইন</Link> করুন।
        </p>
      </main>
    );
  }

  const totals = analytics?.totals;

  return (
    <main>
      <h1>অ্যানালিটিক্স — Analytics</h1>
      <p>
        <Link href="/seller">← ড্যাশবোর্ড</Link>
      </p>
      <p>
        সময়কাল:{" "}
        {DAY_OPTIONS.map((option) => (
          <button
            key={option}
            type="button"
            disabled={option === days}
            onClick={() => setDays(option)}
          >
            {option} দিন
          </button>
        ))}{" "}
        <button type="button" onClick={onDownloadCsv} disabled={csvBusy || !token}>
          CSV ডাউনলোড
        </button>
      </p>
      {message && <p role="alert">{message}</p>}
      {!analytics || !totals ? (
        <p>লোড হচ্ছে…</p>
      ) : (
        <>
          <div className="card">
            <p>
              বিক্রি (সাব-অর্ডার, বাতিল ছাড়া): <strong>{totals.sub_orders}</strong>
              {" · "}পণ্যের মূল্য: <PriceTag amountPoisha={totals.goods_poisha} />
              {" · "}আয়: <PriceTag amountPoisha={totals.earnings_poisha} />
            </p>
            <p>
              ডেলিভারি হয়েছে: <strong>{totals.delivered}</strong>
              {" · "}ফেরত এসেছে: <strong>{totals.returned}</strong>
              {" · "}বাতিল: <strong>{totals.cancelled}</strong>
              {" · "}রিটার্ন হার:{" "}
              <strong>{(totals.return_rate_bps / 100).toFixed(2)}%</strong>
            </p>
            <p>
              <small>
                সংজ্ঞা: পণ্যের মূল্য = বাতিল নয় এমন সাব-অর্ডারের পণ্যমূল্য;
                আয় = শুধু ডেলিভারি হওয়া সাব-অর্ডারের পে-আউট স্ন্যাপশট;
                রিটার্ন হার = ফেরত ÷ (ডেলিভারি + ফেরত)।
              </small>
            </p>
          </div>

          <h2>দৈনিক বিক্রি — Daily</h2>
          <DailyBars
            rows={analytics.daily.map((row) => ({
              date: row.date,
              value: row.goods_poisha,
              title: `${row.date} — ${formatBdt(row.goods_poisha)} · ${row.sub_orders}টি সাব-অর্ডার · আয় ${formatBdt(row.earnings_poisha)}`,
            }))}
          />
          <p>
            <small>
              প্রতিটি বার একটা ঢাকা-সময়ের দিন; বিক্রি না থাকা দিনও শূন্য
              হিসেবে থাকে। বারের উপর মাউস রাখলে সঠিক সংখ্যা দেখা যাবে।
            </small>
          </p>

          <h2>সেরা পণ্য — Top products</h2>
          {analytics.top_products.length === 0 ? (
            <p>এই সময়ে কোনো বিক্রি হয়নি।</p>
          ) : (
            <table className="inventory-table">
              <thead>
                <tr>
                  <th>পণ্য</th>
                  <th>বিক্রি (টি)</th>
                  <th>মূল্য</th>
                </tr>
              </thead>
              <tbody>
                {analytics.top_products.map((product) => (
                  <tr key={product.product_id}>
                    <td>{product.title}</td>
                    <td>{product.units}</td>
                    <td>
                      <PriceTag amountPoisha={product.goods_poisha} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          <p>
            <small>সংজ্ঞা: বাতিল নয় এমন সাব-অর্ডারের আইটেম, পণ্যমূল্যে সাজানো।</small>
          </p>

          <h2>স্ট্যাটাস অনুযায়ী — By status</h2>
          <table className="inventory-table">
            <tbody>
              {analytics.status_breakdown.map((row) => (
                <tr key={row.status}>
                  <td>{STATUS_LABELS[row.status] ?? row.status}</td>
                  <td>{row.count}</td>
                </tr>
              ))}
            </tbody>
          </table>

          <h2>কুরিয়ার অনুযায়ী — By courier</h2>
          {analytics.courier_breakdown.length === 0 ? (
            <p>এই সময়ে কোনো শিপমেন্ট বুক হয়নি।</p>
          ) : (
            <table className="inventory-table">
              <tbody>
                {analytics.courier_breakdown.map((row) => (
                  <tr key={row.courier}>
                    <td>{row.courier}</td>
                    <td>{row.count}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      )}
    </main>
  );
}
