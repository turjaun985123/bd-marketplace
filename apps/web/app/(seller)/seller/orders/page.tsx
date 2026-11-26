"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import {
  listSellerSubOrders,
  updateSubOrderStatus,
  type SellerSubOrder,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/** Sub-order statuses the seller can pick; the API's transition table
 * decides which moves are legal from the current status and rejects
 * the rest with a clear message. */
const STATUSES = [
  "placed",
  "confirmed",
  "shipped",
  "delivered",
  "returned",
  "cancelled",
];

/**
 * Seller sub-orders: every sub-order belonging to the signed-in
 * seller's stores, with items, the money split (including their
 * payout), the buyer's delivery details, and a status control that
 * calls PATCH /v1/seller/sub-orders/{id}/status.
 */
export default function SellerOrdersPage() {
  const { ready, token } = useAuth();
  const [subOrders, setSubOrders] = useState<SellerSubOrder[] | null>(null);
  const [choices, setChoices] = useState<Record<string, string>>({});
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listSellerSubOrders(token, { limit: 50 });
      setSubOrders(list.items);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "অর্ডার লোড করা যায়নি।");
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onUpdateStatus(sub: SellerSubOrder) {
    if (!token) return;
    const next = choices[sub.id] ?? sub.status;
    setMessage(null);
    try {
      const updated = await updateSubOrderStatus(token, sub.id, next);
      setSubOrders((prev) =>
        (prev ?? []).map((s) =>
          s.id === updated.id ? { ...s, status: updated.status } : s
        )
      );
      setMessage("অবস্থা আপডেট হয়েছে। (Status updated.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "আপডেট করা যায়নি।");
    }
  }

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
        <h1>আমার বিক্রির অর্ডার — Seller orders</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>আমার বিক্রির অর্ডার — Seller orders</h1>
      {message && <p role="status">{message}</p>}
      {subOrders === null ? (
        <p>অর্ডার লোড হচ্ছে…</p>
      ) : subOrders.length === 0 ? (
        <p>এখনো কোনো বিক্রির অর্ডার নেই। (No sales orders yet.)</p>
      ) : (
        subOrders.map((sub) => (
          <article key={sub.id} className="card">
            <h3>
              {sub.store_slug} — অবস্থা: {sub.status}
            </h3>
            <p>{new Date(sub.created_at).toLocaleString("bn-BD")}</p>
            {sub.items.map((item) => (
              <p key={item.id}>
                {item.title_snapshot} × {item.quantity} —{" "}
                <PriceTag amountPoisha={item.line_total_poisha} />
              </p>
            ))}
            <p>
              পণ্য: <PriceTag amountPoisha={sub.goods_subtotal_poisha} /> ·
              ডেলিভারি:{" "}
              <PriceTag amountPoisha={sub.delivery_charge_poisha} /> · কমিশন:{" "}
              <PriceTag amountPoisha={sub.commission_poisha} /> · আপনার
              পাওনা:{" "}
              <strong>
                <PriceTag amountPoisha={sub.seller_payout_poisha} />
              </strong>
            </p>
            <p>
              ডেলিভারি: {sub.delivery_name}, {sub.delivery_phone},{" "}
              {sub.delivery_address} ({sub.delivery_zone})
            </p>
            <form
              onSubmit={(e) => {
                e.preventDefault();
                void onUpdateStatus(sub);
              }}
            >
              <label>
                নতুন অবস্থা (status)
                <select
                  value={choices[sub.id] ?? sub.status}
                  onChange={(e) =>
                    setChoices((prev) => ({ ...prev, [sub.id]: e.target.value }))
                  }
                >
                  {STATUSES.map((s) => (
                    <option key={s} value={s}>
                      {s}
                    </option>
                  ))}
                </select>
              </label>
              <button type="submit">আপডেট করুন</button>
            </form>
          </article>
        ))
      )}
    </main>
  );
}
