"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import {
  getFulfillmentLabels,
  type FulfillmentLabels,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

const ZONE_LABELS: Record<string, string> = {
  inside_dhaka: "ঢাকার ভিতরে",
  dhaka_suburb: "ঢাকার উপশহর",
  outside_dhaka: "ঢাকার বাইরে",
};

/**
 * Printable courier labels (Wave 40) — one card per booked
 * sub-order, fed by GET /v1/seller/fulfillment/labels (label
 * DATA only; this page is the rendering). The print rules in
 * globals.css keep each card intact as its own label block.
 * Ids arrive as query params from the fulfillment queue's
 * selection; ids without a booking are reported as skipped
 * with their reasons — never silently dropped.
 */
export default function FulfillmentLabelsPage() {
  const { ready, token } = useAuth();
  const [data, setData] = useState<FulfillmentLabels | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!token) return;
    const params = new URLSearchParams(window.location.search);
    const ids = (params.get("ids") ?? "")
      .split(",")
      .map((id) => id.trim())
      .filter(Boolean);
    const storeId = params.get("store_id") ?? undefined;
    if (ids.length === 0) {
      setError("কোনো সাব-অর্ডার বেছে নেওয়া হয়নি।");
      return;
    }
    let cancelled = false;
    getFulfillmentLabels(token, ids, storeId)
      .then((labels) => {
        if (!cancelled) setData(labels);
      })
      .catch((err) => {
        if (!cancelled) {
          setError(
            err instanceof Error ? err.message : "লেবেল লোড করা যায়নি।"
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

  return (
    <main>
      <div className="no-print">
        <h1>কুরিয়ার লেবেল — Courier labels</h1>
        <p>
          <Link href="/seller/fulfillment">
            ফুলফিলমেন্ট সেন্টারে ফিরুন — Back to fulfillment
          </Link>
        </p>
        {error && <p role="alert">{error}</p>}
        {data !== null && (
          <>
            <p>
              <button type="button" onClick={() => window.print()}>
                প্রিন্ট করুন — Print
              </button>{" "}
              {data.labels.length}টি লেবেল তৈরি হয়েছে।
            </p>
            {data.skipped.length > 0 && (
              <div>
                <h2>যেগুলোর লেবেল হয়নি — Skipped</h2>
                {data.skipped.map((skip, index) => (
                  <p key={index}>
                    {skip.sub_order_id
                      ? `${skip.sub_order_id.slice(0, 8)}…`
                      : "ভুল আইডি"}{" "}
                    — {skip.reason}
                  </p>
                ))}
              </div>
            )}
          </>
        )}
        {data === null && !error && <p>লোড হচ্ছে…</p>}
      </div>

      {data?.labels.map((label) => (
        <article key={label.sub_order_id} className="courier-label">
          <p className="label-from">
            থেকে — From: <strong>{label.store_name}</strong> (
            {label.seller_phone})
          </p>
          <h2>{label.delivery_name}</h2>
          <p>{label.delivery_address}</p>
          {label.delivery_district && (
            <p className="label-district">জেলা — District: {label.delivery_district}</p>
          )}
          {label.order_note && (
            <p className="label-note">নোট — Note: {label.order_note}</p>
          )}
          <p>
            ফোন: <strong>{label.delivery_phone}</strong> · এলাকা:{" "}
            {ZONE_LABELS[label.delivery_zone] ?? label.delivery_zone}
          </p>
          <p className="label-consignment">
            {label.courier_display_name} · {label.consignment_id}
          </p>
          {label.manual_tracking_id && (
            <p>রসিদ নম্বর: {label.manual_tracking_id}</p>
          )}
          <p className="label-cod">
            {label.cod_amount_poisha > 0 ? (
              <>
                ক্যাশ অন ডেলিভারি — সংগ্রহ করুন:{" "}
                <PriceTag amountPoisha={label.cod_amount_poisha} />
              </>
            ) : (
              "কোনো টাকা নেওয়ার নেই — Nothing to collect"
            )}
          </p>
          <p className="label-meta">
            অর্ডার {label.order_id.slice(0, 8)}… · সাব-অর্ডার{" "}
            {label.sub_order_id.slice(0, 8)}… · {label.items_count}টি পণ্য ·{" "}
            {new Date(label.placed_at).toLocaleDateString("bn-BD")}
          </p>
        </article>
      ))}
    </main>
  );
}
