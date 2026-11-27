"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import StoreSwitcher from "@/components/StoreSwitcher";
import {
  bulkBookShipments,
  fulfillmentBucketLabel,
  getCouriers,
  listFulfillment,
  listSellerStores,
  shipmentStatusLabel,
  type BulkBookItemResult,
  type CourierInfo,
  type FulfillmentBucket,
  type FulfillmentQueue,
  type SellerStore,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

const STATUS_LABELS: Record<string, string> = {
  placed: "অর্ডার পড়েছে",
  confirmed: "নিশ্চিত",
  shipped: "পাঠানো হয়েছে",
  delivered: "ডেলিভারি হয়েছে",
  cancelled: "বাতিল",
  returned: "ফেরত এসেছে",
};

const BUCKETS: FulfillmentBucket[] = [
  "to_confirm",
  "to_ship",
  "in_transit",
  "done",
  "attention",
];

/**
 * Seller fulfillment center (Wave 40): ONE work queue over the
 * store's sub-orders — confirm / book / in-transit / done /
 * attention tabs with counts, SLA breach badges, bulk courier
 * booking through the single-booking path (per-item results,
 * never a fake all-success), and label printing for booked
 * parcels. All classification, ages and COD figures come from
 * the API; this page only renders them.
 */
export default function FulfillmentPage() {
  const { ready, token } = useAuth();
  const [queue, setQueue] = useState<FulfillmentQueue | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<FulfillmentBucket>("to_ship");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [couriers, setCouriers] = useState<CourierInfo[]>([]);
  const [courierChoice, setCourierChoice] = useState("auto");
  const [manualCourier, setManualCourier] = useState("sundarban");
  const [manualTrackingId, setManualTrackingId] = useState("");
  const [booking, setBooking] = useState(false);
  const [results, setResults] = useState<BulkBookItemResult[] | null>(null);

  // Store scope (Wave 34 pattern): the switcher selection is
  // shared with the dashboard via localStorage; "" means the
  // API's default — the caller's own earliest store.
  const [stores, setStores] = useState<SellerStore[]>([]);
  const [selectedStoreId, setSelectedStoreId] = useState("");
  const [selectionReady, setSelectionReady] = useState(false);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    (async () => {
      try {
        const list = await listSellerStores(token);
        if (cancelled) return;
        setStores(list);
        const saved = window.localStorage.getItem("seller-store-id");
        if (saved && list.some((store) => store.id === saved)) {
          setSelectedStoreId(saved);
        }
      } catch {
        // The switcher is optional chrome; the queue loads anyway.
      } finally {
        if (!cancelled) setSelectionReady(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [token]);

  useEffect(() => {
    let cancelled = false;
    getCouriers()
      .then((list) => {
        if (!cancelled) setCouriers(list);
      })
      .catch(() => {
        // The courier picker falls back to "auto" only.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const loadQueue = useCallback(async () => {
    if (!token || !selectionReady) return;
    try {
      const data = await listFulfillment(
        token,
        selectedStoreId || undefined
      );
      setQueue(data);
      setError(null);
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "কিউ লোড করা যায়নি।"
      );
    }
  }, [token, selectionReady, selectedStoreId]);

  useEffect(() => {
    void loadQueue();
  }, [loadQueue]);

  function onSelectStore(storeId: string) {
    setSelectedStoreId(storeId);
    setSelected(new Set());
    setResults(null);
    if (storeId) {
      window.localStorage.setItem("seller-store-id", storeId);
    } else {
      window.localStorage.removeItem("seller-store-id");
    }
  }

  function toggle(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }

  const items = queue?.items ?? [];
  const tabItems = items.filter((item) => item.bucket === tab);
  const selectedToShip = items.filter(
    (item) => item.bucket === "to_ship" && selected.has(item.sub_order_id)
  );
  const selectedBooked = items.filter(
    (item) => item.shipment !== null && selected.has(item.sub_order_id)
  );

  async function onBulkBook() {
    if (!token || selectedToShip.length === 0) return;
    setBooking(true);
    setResults(null);
    try {
      const result = await bulkBookShipments(token, {
        storeId: queue?.store_id,
        subOrderIds: selectedToShip.map((item) => item.sub_order_id),
        courier: courierChoice,
        manualCourier:
          courierChoice === "manual" ? manualCourier : undefined,
        manualTrackingId:
          courierChoice === "manual" && manualTrackingId.trim()
            ? manualTrackingId.trim()
            : undefined,
      });
      setResults(result.results);
      setSelected(new Set());
      await loadQueue();
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "বাল্ক বুকিং করা যায়নি।"
      );
    } finally {
      setBooking(false);
    }
  }

  const apiCouriers = couriers.filter(
    (c) => c.mode !== "manual" && c.available
  );
  const manualCouriers = couriers.filter((c) => c.mode === "manual");
  const labelsHref = `/seller/fulfillment/labels?ids=${selectedBooked
    .map((item) => item.sub_order_id)
    .join(",")}${queue ? `&store_id=${queue.store_id}` : ""}`;

  if (!ready) {
    return (
      <main>
        <p>লোড হচ্ছে…</p>
      </main>
    );
  }

  return (
    <main>
      <h1>ফুলফিলমেন্ট সেন্টার — Fulfillment Center</h1>
      <p>
        <Link href="/seller">ড্যাশবোর্ডে ফিরুন — Back to dashboard</Link>
      </p>
      <StoreSwitcher
        stores={stores}
        value={selectedStoreId}
        onChange={onSelectStore}
        includeAll={false}
      />
      {error && <p role="alert">{error}</p>}
      {queue === null ? (
        <p>লোড হচ্ছে…</p>
      ) : (
        <>
          <p>
            স্টোর: <strong>{queue.store_name}</strong> · SLA: কনফার্ম{" "}
            {queue.sla.confirm_hours} ঘণ্টা, বুকিং {queue.sla.ship_hours}{" "}
            ঘণ্টা
            {queue.counts.breached > 0 && (
              <>
                {" "}
                ·{" "}
                <span className="sla-badge">
                  SLA ছাড়িয়েছে {queue.counts.breached}টি
                </span>
              </>
            )}
          </p>

          <p role="tablist">
            {BUCKETS.map((bucket) => (
              <button
                key={bucket}
                type="button"
                role="tab"
                aria-selected={tab === bucket}
                onClick={() => setTab(bucket)}
                style={
                  tab === bucket
                    ? { fontWeight: 700, textDecoration: "underline" }
                    : undefined
                }
              >
                {fulfillmentBucketLabel(bucket)} ({queue.counts[bucket]})
              </button>
            ))}
          </p>

          {tab === "to_ship" && (
            <section className="card">
              <h2>একসঙ্গে বুকিং — Bulk booking</h2>
              <p>
                <label>
                  কুরিয়ার — Courier{" "}
                  <select
                    value={courierChoice}
                    onChange={(e) => setCourierChoice(e.target.value)}
                  >
                    <option value="auto">
                      অটো (সবচেয়ে সস্তা) — Auto (cheapest)
                    </option>
                    {apiCouriers.map((c) => (
                      <option key={c.courier} value={c.courier}>
                        {c.courier}
                        {c.mode === "dev" ? " (ডেভ মোড)" : ""}
                      </option>
                    ))}
                    <option value="manual">ম্যানুয়াল কুরিয়ার — Manual</option>
                  </select>
                </label>{" "}
                {courierChoice === "manual" && (
                  <>
                    <label>
                      ম্যানুয়াল কুরিয়ার{" "}
                      <select
                        value={manualCourier}
                        onChange={(e) => setManualCourier(e.target.value)}
                      >
                        {manualCouriers.map((c) => (
                          <option key={c.courier} value={c.courier}>
                            {c.display_name ?? c.courier}
                          </option>
                        ))}
                      </select>
                    </label>{" "}
                    <label>
                      রসিদ নম্বর (ঐচ্ছিক){" "}
                      <input
                        value={manualTrackingId}
                        onChange={(e) => setManualTrackingId(e.target.value)}
                        placeholder="SB-12345"
                      />
                    </label>
                  </>
                )}
              </p>
              <p>
                <button
                  type="button"
                  onClick={() => void onBulkBook()}
                  disabled={booking || selectedToShip.length === 0}
                >
                  {booking
                    ? "বুকিং চলছে…"
                    : `একসঙ্গে বুক করুন (${selectedToShip.length}টি) — Bulk book`}
                </button>{" "}
                বাম দিকের বাক্স টিক দিয়ে সর্বোচ্চ ২০টি পর্যন্ত একসঙ্গে
                বুক করা যায়। প্রতিটির ফল আলাদা করে দেখানো হবে।
              </p>
              {results !== null && (
                <div>
                  <h3>বুকিংয়ের ফল — Booking results</h3>
                  {results.map((result) => (
                    <p key={result.sub_order_id}>
                      {result.ok ? (
                        <>
                          ✓ বুক হয়েছে — {result.shipment?.courier}
                          {result.shipment?.manual_courier_name
                            ? ` (${result.shipment.manual_courier_name})`
                            : ""}{" "}
                          · কনসাইনমেন্ট: {result.shipment?.consignment_id}
                        </>
                      ) : (
                        <>✗ হয়নি — {result.error}</>
                      )}{" "}
                      <small>({result.sub_order_id.slice(0, 8)}…)</small>
                    </p>
                  ))}
                </div>
              )}
            </section>
          )}

          {selectedBooked.length > 0 && (
            <p>
              <Link href={labelsHref}>
                {selectedBooked.length}টি লেবেল প্রিন্ট করুন — Print labels
              </Link>
            </p>
          )}

          {tabItems.length === 0 ? (
            <p>এই তালিকায় এখন কিছু নেই। (Nothing here right now.)</p>
          ) : (
            tabItems.map((item) => (
              <article key={item.sub_order_id} className="card">
                <p>
                  {(item.bucket === "to_ship" || item.shipment !== null) && (
                    <label>
                      <input
                        type="checkbox"
                        checked={selected.has(item.sub_order_id)}
                        onChange={() => toggle(item.sub_order_id)}
                      />{" "}
                    </label>
                  )}
                  <strong>
                    {STATUS_LABELS[item.status] ?? item.status}
                  </strong>{" "}
                  · {new Date(item.created_at).toLocaleString("bn-BD")} ·{" "}
                  {item.items_count}টি পণ্য · পণ্য{" "}
                  <PriceTag amountPoisha={item.goods_subtotal_poisha} /> +
                  ডেলিভারি{" "}
                  <PriceTag amountPoisha={item.delivery_charge_poisha} />
                  {item.sla_breached && (
                    <>
                      {" "}
                      <span className="sla-badge">
                        SLA ছাড়িয়েছে — {item.age_hours} ঘণ্টা ধরে
                      </span>
                    </>
                  )}
                </p>
                <p>
                  ক্রেতা: {item.delivery_name} ({item.delivery_phone})
                  {item.delivery_district &&
                    ` · জেলা: ${item.delivery_district}`}{" "}
                  · ক্যাশ অন ডেলিভারি:{" "}
                  <strong>
                    <PriceTag amountPoisha={item.cod_due_poisha} />
                  </strong>
                  {item.order_note && (
                    <>
                      <br />
                      ক্রেতার নোট — Buyer note: <strong>{item.order_note}</strong>
                    </>
                  )}
                  {item.order_payment_status === "advance_pending" && (
                    <>
                      {" "}
                      · <span className="sla-badge">অগ্রিম এখনো বাকি</span>
                    </>
                  )}
                </p>
                {item.shipment && (
                  <p>
                    কুরিয়ার:{" "}
                    {item.shipment.manual_courier_name ?? item.shipment.courier}{" "}
                    · কনসাইনমেন্ট: {item.shipment.consignment_id} · অবস্থা:{" "}
                    {shipmentStatusLabel(item.shipment.status)}
                    {item.shipment.manual_tracking_id
                      ? ` · রসিদ: ${item.shipment.manual_tracking_id}`
                      : ""}
                  </p>
                )}
              </article>
            ))
          )}
        </>
      )}
    </main>
  );
}
