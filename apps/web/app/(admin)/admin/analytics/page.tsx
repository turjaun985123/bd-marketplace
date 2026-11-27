"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import DailyBars from "@/components/DailyBars";
import PriceTag from "@/components/PriceTag";
import { getAdminAnalytics, type AdminAnalytics } from "@/lib/api";
import { formatBdt } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

const DAY_OPTIONS = [7, 30, 90];

/**
 * Platform analytics (Wave 27): the whole marketplace from
 * GET /v1/admin/analytics over the last 7/30/90 Asia/Dhaka
 * days. The definitions live on the API and in the README;
 * this page displays them and repeats each one in small
 * print: GMV is goods − discount + delivery over
 * NON-cancelled sub-orders, and commission counts DELIVERED
 * sub-orders only.
 */
export default function AdminAnalyticsPage() {
  return (
    <AdminShell>
      <Analytics />
    </AdminShell>
  );
}

function Analytics() {
  const { token } = useAuth();
  const [days, setDays] = useState(30);
  const [analytics, setAnalytics] = useState<AdminAnalytics | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      setAnalytics(await getAdminAnalytics(token, days));
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "অ্যানালিটিক্স লোড করা যায়নি।"
      );
    }
  }, [token, days]);

  useEffect(() => {
    void load();
  }, [load]);

  if (message) return <p>{message}</p>;
  if (!analytics) return <p>লোড হচ্ছে…</p>;

  const totals = analytics.totals;

  return (
    <>
      <h2>অ্যানালিটিক্স — Platform analytics</h2>
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
        ))}
      </p>

      <div className="card">
        <p>
          GMV: <PriceTag amountPoisha={totals.gmv_poisha} />
          {" · "}কমিশন আয়: <PriceTag amountPoisha={totals.commission_poisha} />
        </p>
        <p>
          <small>
            সংজ্ঞা: GMV = বাতিল নয় এমন সাব-অর্ডারের (পণ্যমূল্য − ছাড় +
            ডেলিভারি চার্জ); কমিশন = শুধু ডেলিভারি হওয়া সাব-অর্ডারের
            (পণ্যমূল্য − ছাড় − বিক্রেতার পে-আউট)।
          </small>
        </p>
      </div>

      <div className="card">
        <p>
          অর্ডার: <strong>{totals.orders}</strong>
          {" · "}সাব-অর্ডার: <strong>{totals.sub_orders}</strong>
          {" · "}নতুন ব্যবহারকারী: <strong>{totals.new_users}</strong>
          {" · "}নতুন স্টোর: <strong>{totals.new_stores}</strong>
        </p>
        <p>
          প্রমোশন চার্জ: <PriceTag amountPoisha={totals.charges_poisha} />
          {" · "}কুপন ছাড়: <PriceTag amountPoisha={totals.coupons.discount_poisha} />{" "}
          ({totals.coupons.redemptions} বার ব্যবহৃত)
        </p>
        <p>
          পে-আউট অনুরোধ: <strong>{totals.payouts.requested_count}</strong> টি —{" "}
          <PriceTag amountPoisha={totals.payouts.requested_poisha} />
          {" · "}পরিশোধিত: <PriceTag amountPoisha={totals.payouts.paid_poisha} />
        </p>
        <p>
          ফলো (Wave 54): মোট <strong>{totals.community.total_follows}</strong>
          {" · "}এই সময়ে নতুন <strong>{totals.community.new_follows}</strong>
          {" · "}নোটিফিকেশন বন্ধ <strong>{totals.community.muted_follows}</strong>
          {" · "}স্টোর কালেকশন <strong>{totals.community.store_collections}</strong>
        </p>
        <p>
          <small>
            সংজ্ঞা: অর্ডার = এই সময়ে তৈরি মূল অর্ডার; সাব-অর্ডার = বাতিলসহ
            সব বিক্রেতা-ভাগ; পে-আউট ও চার্জ = এই সময়ে তৈরি রেকর্ড।
          </small>
        </p>
      </div>

      <h3>দৈনিক GMV — Daily GMV</h3>
      <DailyBars
        rows={analytics.daily.map((row) => ({
          date: row.date,
          value: row.gmv_poisha,
          title: `${row.date} — ${formatBdt(row.gmv_poisha)} · ${row.orders}টি অর্ডার`,
        }))}
      />
      <p>
        <small>প্রতিটি বার একটা ঢাকা-সময়ের দিন; খালি দিনও শূন্য হিসেবে থাকে।</small>
      </p>

      <h3>সেরা স্টোর — Top stores</h3>
      {analytics.top_stores.length === 0 ? (
        <p>এই সময়ে কোনো বিক্রি হয়নি।</p>
      ) : (
        <table className="inventory-table">
          <thead>
            <tr>
              <th>স্টোর</th>
              <th>সাব-অর্ডার</th>
              <th>GMV</th>
            </tr>
          </thead>
          <tbody>
            {analytics.top_stores.map((store) => (
              <tr key={store.store_id}>
                <td>
                  {store.name} ({store.slug})
                </td>
                <td>{store.sub_orders}</td>
                <td>
                  <PriceTag amountPoisha={store.gmv_poisha} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <h3>কুরিয়ার অনুযায়ী — By courier</h3>
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
  );
}
