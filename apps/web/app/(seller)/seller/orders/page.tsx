"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import {
  createShipment,
  getCourierEstimates,
  getCouriers,
  listSellerSubOrders,
  refreshShipment,
  riskLevelLabel,
  shipmentStatusLabel,
  updateSubOrderStatus,
  type CourierEstimates,
  type CourierInfo,
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
 *
 * Wave 6: a confirmed sub-order can be booked with a courier (the
 * courier list comes from GET /v1/couriers — the API decides what is
 * available; unavailable couriers are disabled, dev-simulated ones
 * are labelled). A booked shipment shows its courier, consignment
 * id and unified status, with a tracking refresh button.
 *
 * Wave 7: the courier select gains an "অটো (সবচেয়ে সস্তা)" option —
 * smart routing on the API picks the cheapest available courier and
 * the shipment stores the chosen courier's name. Per-courier
 * "আনুমানিক ৳X" texts come from GET /v1/couriers/estimate priced at
 * the page-level zone of the first confirmed sub-order and a 1 kg
 * parcel: they are the platform tariff's estimates for orientation,
 * not live courier quotes, and each booking is priced by the API
 * from the sub-order's real zone and weight.
 */
export default function SellerOrdersPage() {
  const { ready, token } = useAuth();
  const [subOrders, setSubOrders] = useState<SellerSubOrder[] | null>(null);
  const [choices, setChoices] = useState<Record<string, string>>({});
  const [couriers, setCouriers] = useState<CourierInfo[]>([]);
  const [estimates, setEstimates] = useState<CourierEstimates | null>(null);
  const [courierChoices, setCourierChoices] = useState<Record<string, string>>({});
  const [busyId, setBusyId] = useState<string | null>(null);
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

  useEffect(() => {
    let cancelled = false;
    getCouriers()
      .then((list) => {
        if (!cancelled) setCouriers(list);
      })
      .catch(() => {
        // No courier list: booking controls simply do not render.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Wave 7: per-courier estimates for the option labels, priced
  // once per page at the first confirmed sub-order's zone (falling
  // back to the first sub-order's) and a 1 kg parcel. Purely
  // display orientation — the API prices every booking from the
  // sub-order's real zone and weight.
  useEffect(() => {
    if (estimates !== null || !subOrders || subOrders.length === 0) return;
    const zoneSource =
      subOrders.find((s) => s.status === "confirmed") ?? subOrders[0];
    let cancelled = false;
    getCourierEstimates(zoneSource.delivery_zone, 1000)
      .then((result) => {
        if (!cancelled) setEstimates(result);
      })
      .catch(() => {
        // No estimates: options render without the price texts.
      });
    return () => {
      cancelled = true;
    };
  }, [subOrders, estimates]);

  /** The courier a booking for this sub-order would use: the
   * seller's pick, else "auto" — smart routing picks the cheapest
   * available courier and the shipment records the chosen name. */
  function chosenCourier(sub: SellerSubOrder): string {
    return courierChoices[sub.id] ?? "auto";
  }

  /** The local-tariff estimate text for one courier option, when
   * the estimate endpoint priced it ("আনুমানিক ৳60"). */
  function estimateText(courier: string): string {
    const estimate = estimates?.estimates.find((e) => e.courier === courier);
    if (!estimate || estimate.price_poisha === null) return "";
    const taka = estimate.price_poisha / 100;
    const amount = Number.isInteger(taka) ? String(taka) : taka.toFixed(2);
    return ` — আনুমানিক ৳${amount}`;
  }

  async function onBookCourier(sub: SellerSubOrder) {
    if (!token) return;
    setBusyId(sub.id);
    setMessage(null);
    try {
      await createShipment(token, sub.id, chosenCourier(sub));
      setMessage("কুরিয়ার বুক হয়েছে। (Courier booked.)");
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "বুক করা যায়নি।");
    } finally {
      setBusyId(null);
    }
  }

  async function onRefreshShipment(sub: SellerSubOrder) {
    if (!token || !sub.shipment) return;
    setBusyId(sub.id);
    setMessage(null);
    try {
      await refreshShipment(token, sub.shipment.id);
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "রিফ্রেশ করা যায়নি।");
    } finally {
      setBusyId(null);
    }
  }

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
            <p>
              ক্রেতার ঝুঁকি:{" "}
              <strong>{riskLevelLabel(sub.buyer_risk_level)}</strong> (
              {sub.buyer_risk_level})
              {sub.order_payment_status === "advance_paid" && (
                <>
                  {" "}
                  · অগ্রিম পরিশোধিত:{" "}
                  <PriceTag amountPoisha={sub.advance_paid_poisha} />
                </>
              )}
              {sub.order_payment_status === "advance_pending" && (
                <>
                  {" "}
                  · <strong>অগ্রিম বাকি আছে</strong> (
                  <PriceTag amountPoisha={sub.advance_required_poisha} />)
                  — ক্রেতা অগ্রিম না দিলে কুরিয়ার বুক করা যাবে না।
                </>
              )}
              {sub.advance_required_poisha > 0 &&
                sub.order_payment_status === "pay_on_delivery" && (
                  <>
                    {" "}
                    · অগ্রিম নেওয়া হয়নি (কোনো গেটওয়ে চালু নেই) —
                    অর্ডারটি ঝুঁকিসহ এগিয়েছে।
                  </>
                )}
            </p>
            {sub.shipment ? (
              <p>
                কুরিয়ার: {sub.shipment.courier} · কনসাইনমেন্ট:{" "}
                <code>{sub.shipment.consignment_id}</code> · অবস্থা:{" "}
                {shipmentStatusLabel(sub.shipment.status)} (
                {sub.shipment.status}){" "}
                {!["delivered", "returned", "cancelled"].includes(
                  sub.shipment.status
                ) && (
                  <button
                    type="button"
                    disabled={busyId === sub.id}
                    onClick={() => void onRefreshShipment(sub)}
                  >
                    {busyId === sub.id ? "রিফ্রেশ হচ্ছে…" : "স্ট্যাটাস রিফ্রেশ"}
                  </button>
                )}
              </p>
            ) : sub.status === "confirmed" ? (
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  void onBookCourier(sub);
                }}
              >
                <label>
                  কুরিয়ার (courier)
                  <select
                    value={chosenCourier(sub)}
                    onChange={(e) =>
                      setCourierChoices((prev) => ({
                        ...prev,
                        [sub.id]: e.target.value,
                      }))
                    }
                  >
                    <option value="auto">
                      অটো (সবচেয়ে সস্তা)
                      {estimates?.recommended
                        ? ` — সুপারিশ: ${estimates.recommended}${estimateText(estimates.recommended)}`
                        : ""}
                    </option>
                    {couriers.map((c) => (
                      <option
                        key={c.courier}
                        value={c.courier}
                        disabled={!c.available}
                      >
                        {c.courier}
                        {c.is_default ? " (ডিফল্ট)" : ""}
                        {c.mode === "dev" ? " — ডেভ মোড (পরীক্ষামূলক)" : ""}
                        {!c.available ? " — এখনো চালু হয়নি" : ""}
                        {c.available ? estimateText(c.courier) : ""}
                      </option>
                    ))}
                  </select>
                </label>{" "}
                <button
                  type="submit"
                  disabled={
                    busyId === sub.id || !couriers.some((c) => c.available)
                  }
                >
                  {busyId === sub.id ? "বুক হচ্ছে…" : "কুরিয়ার বুক করুন"}
                </button>
              </form>
            ) : null}
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
