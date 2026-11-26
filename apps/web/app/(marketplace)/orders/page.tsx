"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import { listOrders, type Order } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The buyer's own orders (login-gated), newest first, with the
 * per-seller sub-orders and their fulfilment statuses.
 */
export default function BuyerOrdersPage() {
  const { ready, token } = useAuth();
  const [orders, setOrders] = useState<Order[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    listOrders(token, { limit: 50 })
      .then((list) => {
        if (!cancelled) setOrders(list.items);
      })
      .catch((err) => {
        if (!cancelled) {
          setError(
            err instanceof Error ? err.message : "অর্ডার লোড করা যায়নি।"
          );
        }
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
        <h1>আমার অর্ডার — My orders</h1>
        <p>
          অর্ডার দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>আমার অর্ডার — My orders</h1>
      {error && <p role="alert">{error}</p>}
      {orders === null ? (
        <p>অর্ডার লোড হচ্ছে…</p>
      ) : orders.length === 0 ? (
        <p>
          এখনো কোনো অর্ডার নেই। <Link href="/">পণ্য দেখুন</Link>
        </p>
      ) : (
        orders.map((order) => (
          <article key={order.id} className="card">
            <h3>
              অর্ডার <code>{order.id.slice(0, 8)}…</code> — {order.status}
            </h3>
            <p>{new Date(order.created_at).toLocaleString("bn-BD")}</p>
            {order.sub_orders.map((sub) => (
              <div key={sub.id}>
                <h4>
                  {sub.store_slug} — অবস্থা: {sub.status}
                </h4>
                {sub.items.map((item) => (
                  <p key={item.id}>
                    {item.title_snapshot} × {item.quantity} —{" "}
                    <PriceTag amountPoisha={item.line_total_poisha} />
                  </p>
                ))}
                <p>
                  ডেলিভারি:{" "}
                  <PriceTag amountPoisha={sub.delivery_charge_poisha} />
                </p>
              </div>
            ))}
            <p>
              সর্বমোট:{" "}
              <strong>
                <PriceTag amountPoisha={order.grand_total_poisha} />
              </strong>{" "}
              (ক্যাশ অন ডেলিভারি)
            </p>
          </article>
        ))
      )}
    </main>
  );
}
