"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import { getOrderReceipt, type Receipt } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

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
            : receipt.payment_method}
        </p>

        <h2>ডেলিভারির তথ্য</h2>
        <p>
          {receipt.delivery.name} · {receipt.delivery.phone}
          <br />
          {receipt.delivery.address} ·{" "}
          {ZONE_LABELS[receipt.delivery.zone] ?? receipt.delivery.zone}
        </p>

        {receipt.sub_orders.map((sub) => (
          <section key={sub.store_slug}>
            <h2>
              {sub.store_name} ({sub.store_slug}) — {sub.status}
            </h2>
            {sub.items.map((item) => (
              <p key={item.id}>
                {item.title_snapshot} × {item.quantity} —{" "}
                <PriceTag amountPoisha={item.line_total_poisha} />
              </p>
            ))}
            <p>
              পণ্যের উপমোট:{" "}
              <PriceTag amountPoisha={sub.goods_subtotal_poisha} /> ·
              ডেলিভারি চার্জ:{" "}
              <PriceTag amountPoisha={sub.delivery_charge_poisha} />
            </p>
          </section>
        ))}

        <h2>মোট হিসাব</h2>
        <p>
          পণ্যের মোট: <PriceTag amountPoisha={receipt.goods_total_poisha} />
          <br />
          ডেলিভারি মোট:{" "}
          <PriceTag amountPoisha={receipt.delivery_total_poisha} />
          <br />
          সর্বমোট:{" "}
          <strong>
            <PriceTag amountPoisha={receipt.grand_total_poisha} />
          </strong>
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
