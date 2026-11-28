"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import ProductImage from "@/components/ProductImage";
import {
  getOrderReceipt,
  paymentStatusLabel,
  type Receipt,
  type TimelineEvent,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";
import OrderTimeline from "@/components/OrderTimeline";

/**
 * The courier's latest unified status on a sub-order, read
 * off its timeline (the receipt carries no shipment object;
 * the timeline's shipment_status events are the courier
 * leg). Null until the courier reports anything.
 */
function lastShipmentStatus(events: TimelineEvent[]): string | null {
  let status: string | null = null;
  for (const event of events) {
    if (event.kind === "shipment_status") status = event.status;
  }
  return status;
}

/** Zone values with their Bangla labels (display only). */
const ZONE_LABELS: Record<string, string> = {
  inside_dhaka: "ঢাকার ভিতরে",
  dhaka_suburb: "ঢাকার উপশহর",
  outside_dhaka: "ঢাকার বাইরে",
};

/**
 * Order detail + printable receipt (Wave 4). All data comes from the
 * receipt endpoint's view-model — the API has already left out
 * commission and payout, so this page can never show them. The Print
 * button uses window.print(); print CSS hides it and the back link.
 *
 * Wave 55: the page doubles as the buyer's tracking view —
 * each store's section gains the shared OrderTimeline
 * (stepper + event list from the receipt's per-sub-order
 * timeline), and the buyer's order note shows under the
 * delivery block.
 */
export default function OrderReceiptPage({
  params,
}: {
  params: { id: string };
}) {
  const { ready, token } = useAuth();
  const [receipt, setReceipt] = useState<Receipt | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getOrderReceipt(token, params.id)
      .then((data) => {
        if (!cancelled) setReceipt(data);
      })
      .catch((err) => {
        if (!cancelled) {
          setError(
            err instanceof Error ? err.message : "রসিদ লোড করা যায়নি।"
          );
        }
      });
    return () => {
      cancelled = true;
    };
  }, [token, params.id]);

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
        <h1>রসিদ — Receipt</h1>
        <p>
          রসিদ দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  if (error) {
    return (
      <main>
        <h1>রসিদ — Receipt</h1>
        <p role="alert">{error}</p>
        <p>
          <Link href="/orders">আমার অর্ডারে ফিরে যান</Link>
        </p>
      </main>
    );
  }

  if (receipt === null) {
    return (
      <main>
        <p>রসিদ লোড হচ্ছে…</p>
      </main>
    );
  }

  return (
    <main>
      <p className="no-print">
        <Link href="/orders">← আমার অর্ডার</Link>
      </p>
      <article className="receipt">
        <h1>{receipt.platform_name}</h1>
        <p>
          রসিদ — Receipt · অর্ডার নম্বর: <code>{receipt.order_id}</code>
        </p>
        <p>
          তারিখ: {new Date(receipt.created_at).toLocaleString("bn-BD")} ·
          অবস্থা: {receipt.status} · পেমেন্ট:{" "}
          {receipt.payment_method === "cod"
            ? "ক্যাশ অন ডেলিভারি (COD)"
            : receipt.payment_method}{" "}
          — {paymentStatusLabel(receipt.payment_status)}
        </p>

        <h2>ডেলিভারির তথ্য</h2>
        <p>
          {receipt.delivery.name} · {receipt.delivery.phone}
          <br />
          {receipt.delivery.address} ·{" "}
          {ZONE_LABELS[receipt.delivery.zone] ?? receipt.delivery.zone}
        </p>
        {receipt.order_note && (
          <p>আপনার নোট — Your note for the sellers: {receipt.order_note}</p>
        )}

        {receipt.sub_orders.map((sub) => (
          <section key={sub.store_slug}>
            <h2>
              {sub.store_name} ({sub.store_slug}) — {sub.status}
            </h2>
            {sub.items.map((item) => (
              <p key={item.id} className="order-item-line">
                <ProductImage
                  src={item.image_url}
                  alt={item.title_snapshot}
                  placeholderText={item.title_snapshot}
                  className="order-item-image"
                />
                <span>
                  {item.title_snapshot} × {item.quantity} —{" "}
                  <PriceTag amountPoisha={item.line_total_poisha} />
                </span>
              </p>
            ))}
            <p>
              পণ্যের উপমোট:{" "}
              <PriceTag amountPoisha={sub.goods_subtotal_poisha} /> ·
              ডেলিভারি চার্জ:{" "}
              <PriceTag amountPoisha={sub.delivery_charge_poisha} />
            </p>
            <h3>ট্র্যাকিং — Tracking</h3>
            <OrderTimeline
              status={sub.status}
              shipmentStatus={lastShipmentStatus(sub.timeline)}
              events={sub.timeline}
              steps={sub.steps}
            />
          </section>
        ))}

        <h2>মোট হিসাব</h2>
        <p>
          পণ্যের মোট: <PriceTag amountPoisha={receipt.goods_total_poisha} />
          {receipt.discount_total_poisha > 0 && (
            <>
              <br />
              কুপন ছাড় ({receipt.coupon_code}): −
              <PriceTag amountPoisha={receipt.discount_total_poisha} />
            </>
          )}
          {receipt.loyalty_discount_poisha > 0 && (
            <>
              <br />
              লয়্যালটি ছাড় ({receipt.loyalty_points_used} পয়েন্ট): −
              <PriceTag amountPoisha={receipt.loyalty_discount_poisha} />
            </>
          )}
          {receipt.gift_card_discount_poisha > 0 && (
            <>
              <br />
              গিফট কার্ড ছাড় (••{receipt.gift_card_last4}): −
              <PriceTag amountPoisha={receipt.gift_card_discount_poisha} />
            </>
          )}
          <br />
          ডেলিভারি মোট:{" "}
          <PriceTag amountPoisha={receipt.delivery_total_poisha} />
          <br />
          সর্বমোট:{" "}
          <strong>
            <PriceTag amountPoisha={receipt.grand_total_poisha} />
          </strong>
          {receipt.advance_required_poisha > 0 && (
            <>
              <br />
              অগ্রিম: <PriceTag amountPoisha={receipt.advance_required_poisha} />
              {receipt.advance_paid_poisha > 0 ? (
                <>
                  {" "}
                  (পরিশোধিত:{" "}
                  <PriceTag amountPoisha={receipt.advance_paid_poisha} />)
                </>
              ) : (
                " (এখনো পরিশোধ হয়নি)"
              )}
            </>
          )}
        </p>
        {receipt.cod_amount_due_poisha > 0 && (
          <p>
            <strong>
              ডেলিভারির সময় পরিশোধ করতে হবে:{" "}
              <PriceTag amountPoisha={receipt.cod_amount_due_poisha} />
            </strong>
          </p>
        )}
      </article>
      <p className="no-print">
        <button type="button" onClick={() => window.print()}>
          রসিদ প্রিন্ট করুন — Print receipt
        </button>
      </p>
    </main>
  );
}
