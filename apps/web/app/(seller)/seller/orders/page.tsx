"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import ProductImage from "@/components/ProductImage";
import {
  createManualShipment,
  createShipment,
  getCourierEstimates,
  getCouriers,
  listSellerSubOrders,
  refreshShipment,
  riskLevelLabel,
  setManualShipmentStatus,
  setManualTracking,
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
 * Legal next statuses for a MANUAL shipment (Wave 19), mirroring
 * the API's domain transition table (can_transition_shipment):
 * the forward walk, the partial-delivery branch, the return
 * path, and cancelling a parcel that has not reached a final
 * state. Display only — the API re-checks every move and a
 * disagreement surfaces as its 409 message.
 */
const SHIPMENT_NEXT_STATUSES: Record<string, string[]> = {
  created: ["picked_up", "cancelled"],
  picked_up: ["in_transit", "cancelled"],
  in_transit: ["out_for_delivery", "cancelled"],
  out_for_delivery: ["delivered", "partial_delivered", "cancelled"],
  partial_delivered: ["delivered", "returned", "cancelled"],
  delivered: ["return_in_transit"],
  return_in_transit: ["returned", "cancelled"],
  returned: [],
  cancelled: [],
  unknown: [],
};

/** The select value prefix marking a manual courier pick. */
const MANUAL_PREFIX = "manual:";

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
 *
 * Wave 19: the select gains a "ম্যানুয়াল কুরিয়ার" group —
 * Sundarban, SA Paribahan, Karatoa, Carrybee have no APIs, so a
 * manual booking just records the shipment (with the counter
 * receipt number when the seller has it). A manual shipment's
 * card replaces the refresh button with seller-entered status
 * buttons (only the legal next statuses) and a receipt-number
 * edit field: the seller IS the tracking feed for these.
 */
export default function SellerOrdersPage() {
  const { ready, token } = useAuth();
  const [subOrders, setSubOrders] = useState<SellerSubOrder[] | null>(null);
  const [choices, setChoices] = useState<Record<string, string>>({});
  const [couriers, setCouriers] = useState<CourierInfo[]>([]);
  const [estimates, setEstimates] = useState<CourierEstimates | null>(null);
  const [courierChoices, setCourierChoices] = useState<Record<string, string>>({});
  // Wave 19: the receipt number typed into a manual booking form
  // (per sub-order) and into a manual shipment's edit field (per
  // shipment).
  const [manualTrackingInputs, setManualTrackingInputs] = useState<
    Record<string, string>
  >({});
  const [manualTrackingEdits, setManualTrackingEdits] = useState<
    Record<string, string>
  >({});
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
      const chosen = chosenCourier(sub);
      if (chosen.startsWith(MANUAL_PREFIX)) {
        // Wave 19: a manual booking records the shipment
        // directly — the key after the prefix names the courier
        // and the receipt number goes along when already known.
        const receipt = manualTrackingInputs[sub.id]?.trim();
        await createManualShipment(
          token,
          sub.id,
          chosen.slice(MANUAL_PREFIX.length),
          receipt ? receipt : undefined
        );
        setMessage(
          "ম্যানুয়াল কুরিয়ার বুকিং রেকর্ড হয়েছে। (Manual booking recorded.)"
        );
      } else {
        await createShipment(token, sub.id, chosen);
        setMessage("কুরিয়ার বুক হয়েছে। (Courier booked.)");
      }
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "বুক করা যায়নি।");
    } finally {
      setBusyId(null);
    }
  }

  /** Wave 19: record one seller-entered status for a manual
   * shipment; the API validates the move and syncs the
   * sub-order, then the list reloads from the server. */
  async function onManualStatus(sub: SellerSubOrder, next: string) {
    if (!token || !sub.shipment) return;
    setBusyId(sub.id);
    setMessage(null);
    try {
      await setManualShipmentStatus(token, sub.shipment.id, next);
      setMessage("শিপমেন্টের অবস্থা আপডেট হয়েছে। (Shipment status updated.)");
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "আপডেট করা যায়নি।");
    } finally {
      setBusyId(null);
    }
  }

  /** Wave 19: save the counter receipt number typed on a manual
   * shipment's card. */
  async function onSaveManualTracking(sub: SellerSubOrder) {
    if (!token || !sub.shipment) return;
    const value = (
      manualTrackingEdits[sub.shipment.id] ??
      sub.shipment.manual_tracking_id ??
      ""
    ).trim();
    if (!value) {
      setMessage("আগে রসিদ নম্বরটি লিখুন। (Enter the receipt number first.)");
      return;
    }
    setBusyId(sub.id);
    setMessage(null);
    try {
      await setManualTracking(token, sub.shipment.id, value);
      setMessage("রসিদ নম্বর সেভ হয়েছে। (Receipt number saved.)");
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "সেভ করা যায়নি।");
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

  // Wave 19: GET /v1/couriers mixes API couriers with the four
  // manual ones (mode "manual"); the booking select renders them
  // in separate groups because they book completely differently.
  const apiCouriers = couriers.filter((c) => c.mode !== "manual");
  const manualCouriers = couriers.filter((c) => c.mode === "manual");

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
            {(sub.status === "delivered" || sub.status === "returned") && (
              <p>
                <Link href={`/invoice/${sub.id}?as=seller`}>
                  ইনভয়েস দেখুন — Invoice
                </Link>
              </p>
            )}
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
              sub.shipment.courier === "manual" ? (
                <div>
                  <p>
                    ম্যানুয়াল কুরিয়ার:{" "}
                    {sub.shipment.manual_courier_name ?? "manual"} ·
                    রসিদ নম্বর:{" "}
                    {sub.shipment.manual_tracking_id ? (
                      <code>{sub.shipment.manual_tracking_id}</code>
                    ) : (
                      "এখনো দেওয়া হয়নি"
                    )}{" "}
                    · অবস্থা: {shipmentStatusLabel(sub.shipment.status)} (
                    {sub.shipment.status})
                  </p>
                  <p>
                    এই কুরিয়ারের কোনো API নেই — পার্সেলের অবস্থা
                    নিজে আপডেট করুন:
                  </p>
                  {(SHIPMENT_NEXT_STATUSES[sub.shipment.status] ?? []).map(
                    (next) => (
                      <button
                        key={next}
                        type="button"
                        disabled={busyId === sub.id}
                        onClick={() => void onManualStatus(sub, next)}
                      >
                        {shipmentStatusLabel(next)} ({next})
                      </button>
                    )
                  )}{" "}
                  <form
                    onSubmit={(e) => {
                      e.preventDefault();
                      void onSaveManualTracking(sub);
                    }}
                  >
                    <label>
                      কাউন্টার রসিদ নম্বর (receipt number)
                      <input
                        value={
                          manualTrackingEdits[sub.shipment.id] ??
                          sub.shipment.manual_tracking_id ??
                          ""
                        }
                        onChange={(e) =>
                          setManualTrackingEdits((prev) => ({
                            ...prev,
                            [sub.shipment!.id]: e.target.value,
                          }))
                        }
                        placeholder="যেমন: SB-12345"
                      />
                    </label>{" "}
                    <button type="submit" disabled={busyId === sub.id}>
                      রসিদ নম্বর সেভ করুন
                    </button>
                  </form>
                </div>
              ) : (
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
              )
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
                    {apiCouriers.map((c) => (
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
                    {manualCouriers.length > 0 && (
                      <optgroup label="ম্যানুয়াল কুরিয়ার — কাউন্টারে বুকিং (API নেই)">
                        {manualCouriers.map((c) => (
                          <option
                            key={c.courier}
                            value={`${MANUAL_PREFIX}${c.courier}`}
                          >
                            {c.display_name ?? c.courier}
                          </option>
                        ))}
                      </optgroup>
                    )}
                  </select>
                </label>{" "}
                {chosenCourier(sub).startsWith(MANUAL_PREFIX) && (
                  <label>
                    কাউন্টার রসিদ নম্বর (থাকলে)
                    <input
                      value={manualTrackingInputs[sub.id] ?? ""}
                      onChange={(e) =>
                        setManualTrackingInputs((prev) => ({
                          ...prev,
                          [sub.id]: e.target.value,
                        }))
                      }
                      placeholder="যেমন: SB-12345"
                    />
                  </label>
                )}{" "}
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
