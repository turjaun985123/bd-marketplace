"use client";

import { useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import {
  paymentStatusLabel,
  shipmentStatusLabel,
  trackOrder,
  type TrackedOrder,
} from "@/lib/api";

/** Bangla labels for sub-order statuses (display only). */
const SUB_ORDER_LABELS: Record<string, string> = {
  placed: "অর্ডার পড়েছে",
  confirmed: "বিক্রেতা নিশ্চিত করেছেন",
  shipped: "পাঠানো হয়েছে",
  delivered: "ডেলিভারি হয়েছে",
  returned: "ফেরত গেছে",
  cancelled: "বাতিল হয়েছে",
};

/**
 * Public order tracking (Wave 16). No login: the order id plus
 * the delivery phone given at checkout are the credential — the
 * link is shareable with family or opened from an SMS. The API
 * answers every failure (unknown id, wrong phone) with the same
 * 404, so this page shows ONE "খুঁজে পাওয়া যায়নি" message and
 * never hints whether the order itself exists.
 */
export default function TrackPage() {
  const [orderId, setOrderId] = useState("");
  const [phone, setPhone] = useState("");
  const [tracked, setTracked] = useState<TrackedOrder | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function onTrack(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setTracked(null);
    try {
      const result = await trackOrder(orderId.trim(), phone.trim());
      setTracked(result);
    } catch {
      // The API deliberately gives one answer for every failure;
      // mirror it with one message here.
      setError(
        "অর্ডারটি খুঁজে পাওয়া যায়নি। অর্ডার নম্বর ও ডেলিভারির মোবাইল নম্বর মিলিয়ে আবার চেষ্টা করুন।"
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <main>
      <h1>অর্ডার ট্র্যাক করুন — Track your order</h1>
      <p>
        অর্ডার নম্বর আর অর্ডারের সময় দেওয়া মোবাইল নম্বরটি দিন —
        লগইন লাগবে না। (Enter the order number and the mobile
        number given at checkout — no login needed.)
      </p>
      <form onSubmit={(e) => void onTrack(e)}>
        <label>
          অর্ডার নম্বর (order number)
          <input
            value={orderId}
            onChange={(e) => setOrderId(e.target.value)}
            placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
            required
          />
        </label>{" "}
        <label>
          মোবাইল নম্বর (mobile number)
          <input
            value={phone}
            onChange={(e) => setPhone(e.target.value)}
            placeholder="01XXXXXXXXX"
            inputMode="tel"
            required
          />
        </label>{" "}
        <button type="submit" disabled={busy}>
          {busy ? "খোঁজা হচ্ছে…" : "ট্র্যাক করুন"}
        </button>
      </form>

      {error && <p role="alert">{error}</p>}

      {tracked && (
        <>
          <h2>
            অর্ডার <code>{tracked.order_id}</code>
          </h2>
          <p>
            তারিখ: {new Date(tracked.placed_at).toLocaleString("bn-BD")} ·
            পেমেন্ট: {paymentStatusLabel(tracked.payment_status)}
            {tracked.payment_method !== "cod" &&
              ` (${tracked.payment_method})`}{" "}
            · সর্বমোট:{" "}
            <strong>
              <PriceTag amountPoisha={tracked.grand_total_poisha} />
            </strong>
          </p>
          {tracked.sub_orders.map((sub) => (
            <article key={sub.store_slug} className="card">
              <h3>
                {sub.store_slug} —{" "}
                {SUB_ORDER_LABELS[sub.status] ?? sub.status}
              </h3>
              {sub.items.map((item, idx) => (
                <p key={idx}>
                  {item.title_snapshot} × {item.quantity}
                </p>
              ))}
              {sub.shipment ? (
                <>
                  <p>
                    কুরিয়ার: {sub.shipment.courier} · কনসাইনমেন্ট:{" "}
                    <code>{sub.shipment.consignment_id}</code> · অবস্থা:{" "}
                    {shipmentStatusLabel(sub.shipment.status)}
                  </p>
                  <h4>যাত্রার বিবরণ — Timeline</h4>
                  <ol>
                    {sub.shipment.events.map((event, idx) => (
                      <li key={idx}>
                        {shipmentStatusLabel(event.status)}
                        {event.note && <> ({event.note})</>} —{" "}
                        {new Date(event.at).toLocaleString("bn-BD")}
                      </li>
                    ))}
                  </ol>
                </>
              ) : (
                <p>কুরিয়ার এখনো বুক হয়নি।</p>
              )}
            </article>
          ))}
        </>
      )}

      <p>
        <Link href="/">মার্কেটপ্লেসে ফিরে যান</Link>
      </p>
    </main>
  );
}
