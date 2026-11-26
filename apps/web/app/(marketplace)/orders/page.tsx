"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import {
  cancelOrder,
  createReturnRequest,
  downloadBuyerExport,
  listBuyerReturnRequests,
  listOrders,
  paymentStatusLabel,
  refreshShipment,
  refundStatusLabel,
  retryPayment,
  returnStatusLabel,
  shipmentStatusLabel,
  type Order,
  type ReturnRequest,
  type SubOrder,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The buyer's own orders (login-gated), newest first, with the
 * per-seller sub-orders and their fulfilment statuses. An order can be
 * cancelled from here while EVERY sub-order is still placed (Wave 4) —
 * the button mirrors that rule, but the server is the source of truth:
 * a 409 answer is shown as an error and nothing changes locally.
 *
 * Wave 16: a DELIVERED sub-order can be returned from here — the
 * buyer gives a reason, the seller approves or rejects, and once the
 * goods are received back the refund (goods minus any coupon
 * discount; delivery is never refunded) shows as pending until the
 * platform records it done. The sub-order's `return_status` summary
 * comes with the order; the refund figures come from the buyer's
 * return-request list, keyed by sub-order.
 *
 * Wave 22: a delivered or returned sub-order links to its
 * printable per-store invoice (the seller's document for that
 * slice; the receipt link below stays the whole-order summary).
 *
 * Wave 31: the header button downloads the buyer's own record
 * (GET /v1/buyer/export — profile, orders, reviews, wishlist,
 * questions) as one JSON file, the portability export.
 */
export default function BuyerOrdersPage() {
  const { ready, token } = useAuth();
  const [orders, setOrders] = useState<Order[] | null>(null);
  const [returnsBySubOrder, setReturnsBySubOrder] = useState<
    Record<string, ReturnRequest>
  >({});
  const [error, setError] = useState<string | null>(null);
  const [cancellingId, setCancellingId] = useState<string | null>(null);
  const [retryingId, setRetryingId] = useState<string | null>(null);
  const [refreshingId, setRefreshingId] = useState<string | null>(null);
  const [returnReasons, setReturnReasons] = useState<Record<string, string>>(
    {}
  );
  const [returningId, setReturningId] = useState<string | null>(null);
  const [exportBusy, setExportBusy] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);

  // Wave 31: download the buyer's own record as one JSON file.
  async function onDownloadExport() {
    if (!token) return;
    setExportBusy(true);
    setExportError(null);
    try {
      const blob = await downloadBuyerExport(token);
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = "bd-marketplace-buyer-export.json";
      anchor.click();
      URL.revokeObjectURL(url);
    } catch (err) {
      setExportError(
        err instanceof Error ? err.message : "এক্সপোর্ট ডাউনলোড করা যায়নি।"
      );
    } finally {
      setExportBusy(false);
    }
  }

  // Reload orders + the buyer's return requests together; both
  // lists are re-read after any action so what is shown is always
  // the server's state.
  async function reload() {
    if (!token) return;
    const [list, returns] = await Promise.all([
      listOrders(token, { limit: 50 }),
      listBuyerReturnRequests(token, { limit: 50 }),
    ]);
    setOrders(list.items);
    setReturnsBySubOrder(
      Object.fromEntries(returns.items.map((r) => [r.sub_order_id, r]))
    );
  }

  // Ask to return one delivered sub-order (Wave 16). The API
  // enforces the rules (delivered only, one request per
  // sub-order); its error is shown as-is.
  async function onRequestReturn(sub: SubOrder) {
    if (!token) return;
    const reason = (returnReasons[sub.id] ?? "").trim();
    if (!reason) {
      setError("রিটার্নের কারণ লিখুন। (Give a reason for the return.)");
      return;
    }
    setReturningId(sub.id);
    setError(null);
    try {
      await createReturnRequest(token, sub.id, reason);
      await reload();
    } catch (err) {
      setError(
        `রিটার্নের আবেদন করা যায়নি। ${
          err instanceof Error ? err.message : ""
        }`.trim()
      );
    } finally {
      setReturningId(null);
    }
  }

  // Refresh one sub-order's courier tracking (Wave 6): the API polls
  // the courier and applies any legal status change; the list is
  // then reloaded so what is shown is the server's state.
  async function onRefreshShipment(sub: SubOrder) {
    if (!token || !sub.shipment) return;
    setRefreshingId(sub.shipment.id);
    setError(null);
    try {
      await refreshShipment(token, sub.shipment.id);
      const list = await listOrders(token, { limit: 50 });
      setOrders(list.items);
    } catch (err) {
      setError(
        `ট্র্যাকিং রিফ্রেশ করা যায়নি। ${
          err instanceof Error ? err.message : ""
        }`.trim()
      );
    } finally {
      setRefreshingId(null);
    }
  }

  // Retry a failed online payment (Wave 5): the API moves it back to
  // pending; the list is then reloaded so the status shown is the
  // server's, not an optimistic guess.
  async function onRetryPayment(order: Order) {
    if (!token || !order.payment) return;
    setRetryingId(order.id);
    setError(null);
    try {
      await retryPayment(token, order.payment.id);
      const list = await listOrders(token, { limit: 50 });
      setOrders(list.items);
    } catch (err) {
      setError(
        `পেমেন্ট আবার চেষ্টা করা যায়নি। ${
          err instanceof Error ? err.message : ""
        }`.trim()
      );
    } finally {
      setRetryingId(null);
    }
  }

  async function onCancel(orderId: string) {
    if (!token) return;
    setCancellingId(orderId);
    setError(null);
    try {
      const updated = await cancelOrder(token, orderId);
      setOrders((prev) =>
        (prev ?? []).map((o) => (o.id === updated.id ? updated : o))
      );
    } catch (err) {
      setError(
        `অর্ডার বাতিল করা যায়নি। ${
          err instanceof Error ? err.message : ""
        }`.trim()
      );
    } finally {
      setCancellingId(null);
    }
  }

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    Promise.all([
      listOrders(token, { limit: 50 }),
      listBuyerReturnRequests(token, { limit: 50 }),
    ])
      .then(([list, returns]) => {
        if (cancelled) return;
        setOrders(list.items);
        setReturnsBySubOrder(
          Object.fromEntries(returns.items.map((r) => [r.sub_order_id, r]))
        );
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
      <p>
        <button
          type="button"
          onClick={() => void onDownloadExport()}
          disabled={exportBusy}
        >
          {exportBusy ? "তৈরি হচ্ছে…" : "ডেটা এক্সপোর্ট — Download my data (JSON)"}
        </button>{" "}
        আপনার প্রোফাইল, অর্ডার, রিভিউ, উইশলিস্ট আর প্রশ্ন — সব এক
        ফাইলে।
      </p>
      {exportError && <p role="alert">{exportError}</p>}
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
                    {item.title_snapshot}
                    {item.variant_name && <> ({item.variant_name})</>} ×{" "}
                    {item.quantity} —{" "}
                    <PriceTag amountPoisha={item.line_total_poisha} />
                  </p>
                ))}
                <p>
                  ডেলিভারি:{" "}
                  <PriceTag amountPoisha={sub.delivery_charge_poisha} />
                </p>
                {sub.shipment && (
                  <p>
                    কুরিয়ার: {sub.shipment.courier} · কনসাইনমেন্ট:{" "}
                    <code>{sub.shipment.consignment_id}</code> · অবস্থা:{" "}
                    {shipmentStatusLabel(sub.shipment.status)}{" "}
                    {!["delivered", "returned", "cancelled"].includes(
                      sub.shipment.status
                    ) && (
                      <button
                        type="button"
                        disabled={refreshingId === sub.shipment.id}
                        onClick={() => void onRefreshShipment(sub)}
                      >
                        {refreshingId === sub.shipment.id
                          ? "রিফ্রেশ হচ্ছে…"
                          : "স্ট্যাটাস রিফ্রেশ"}
                      </button>
                    )}
                  </p>
                )}
                {(sub.status === "delivered" ||
                  sub.status === "returned") && (
                  <p>
                    <Link href={`/invoice/${sub.id}`}>
                      ইনভয়েস দেখুন — Invoice ({sub.store_slug})
                    </Link>
                  </p>
                )}
                {sub.return_status ? (
                  <p>
                    রিটার্ন: {returnStatusLabel(sub.return_status)}
                    {(() => {
                      const ret = returnsBySubOrder[sub.id];
                      if (!ret) return null;
                      return (
                        <>
                          {" "}
                          · রিফান্ড: {refundStatusLabel(ret.refund_status)}
                          {ret.refund_poisha !== null && (
                            <>
                              {" "}
                              (
                              <PriceTag amountPoisha={ret.refund_poisha} />)
                            </>
                          )}
                          {ret.seller_note && (
                            <> · বিক্রেতার নোট: {ret.seller_note}</>
                          )}
                        </>
                      );
                    })()}
                  </p>
                ) : (
                  sub.status === "delivered" && (
                    <p>
                      <input
                        value={returnReasons[sub.id] ?? ""}
                        onChange={(e) =>
                          setReturnReasons((prev) => ({
                            ...prev,
                            [sub.id]: e.target.value,
                          }))
                        }
                        placeholder="রিটার্নের কারণ (যেমন: পণ্য ছিঁড়ে গেছে)"
                      />{" "}
                      <button
                        type="button"
                        disabled={returningId === sub.id}
                        onClick={() => void onRequestReturn(sub)}
                      >
                        {returningId === sub.id
                          ? "আবেদন হচ্ছে…"
                          : "রিটার্নের আবেদন করুন"}
                      </button>
                    </p>
                  )
                )}
              </div>
            ))}
            <p>
              সর্বমোট:{" "}
              <strong>
                <PriceTag amountPoisha={order.grand_total_poisha} />
              </strong>{" "}
              · পেমেন্ট: {paymentStatusLabel(order.payment_status)}
              {order.payment_method !== "cod" &&
                ` (${order.payment_method})`}
            </p>
            {order.payment_status === "failed" && order.payment && (
              <p>
                <button
                  type="button"
                  disabled={retryingId === order.id}
                  onClick={() => void onRetryPayment(order)}
                >
                  {retryingId === order.id
                    ? "চেষ্টা হচ্ছে…"
                    : "পেমেন্ট আবার চেষ্টা করুন — Retry payment"}
                </button>
              </p>
            )}
            <p>
              <Link href={`/orders/${order.id}`}>
                রসিদ / বিবরণ দেখুন — Receipt & details
              </Link>
            </p>
            {order.sub_orders.length > 0 &&
              order.sub_orders.every((sub) => sub.status === "placed") && (
                <p>
                  <button
                    type="button"
                    disabled={cancellingId === order.id}
                    onClick={() => void onCancel(order.id)}
                  >
                    {cancellingId === order.id
                      ? "বাতিল হচ্ছে…"
                      : "অর্ডার বাতিল করুন — Cancel order"}
                  </button>{" "}
                  বাতিল করলে সব পণ্য স্টকে ফিরে যাবে।
                </p>
              )}
          </article>
        ))
      )}
    </main>
  );
}
