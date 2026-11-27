"use client";

import { useEffect, useState } from "react";
import PriceTag from "./PriceTag";
import {
  getDeliveryEstimate,
  type DeliveryEstimate as Estimate,
  type DeliveryZoneKey,
} from "@/lib/api";

/** The zone choices, in the order a buyer thinks of them. */
const ZONES: { key: DeliveryZoneKey; label: string }[] = [
  { key: "inside_dhaka", label: "ঢাকার ভিতরে — Inside Dhaka" },
  { key: "dhaka_suburb", label: "ঢাকার উপশহর — Dhaka Suburb" },
  { key: "outside_dhaka", label: "ঢাকার বাইরে — Outside Dhaka" },
];

/** The localStorage key the buyer's zone choice persists under. */
const ZONE_STORAGE_KEY = "bdm-delivery-zone";

function isZoneKey(value: string | null): value is DeliveryZoneKey {
  return ZONES.some((zone) => zone.key === value);
}

/** The buyer's remembered zone, or Inside Dhaka before they choose. */
export function readStoredZone(): DeliveryZoneKey {
  if (typeof window === "undefined") return "inside_dhaka";
  const stored = window.localStorage.getItem(ZONE_STORAGE_KEY);
  return isZoneKey(stored) ? stored : "inside_dhaka";
}

/**
 * The product page's delivery block (Wave 54): pick a
 * zone, see the REAL charge for one unit of this product
 * (the API's estimate endpoint — the Wave 1 charge rule
 * checkout itself bills by) and the platform's published
 * day range for the zone. The choice persists on the
 * device (localStorage — it is a browsing preference,
 * not account data). The copy is precise about what the
 * estimate covers: the courier leg for this product
 * alone, at the weight the seller declared; a multi-item
 * cart is priced at checkout by the same rule over the
 * whole parcel.
 */
export default function DeliveryEstimate({
  weightGrams,
}: {
  weightGrams: number;
}) {
  const [zone, setZone] = useState<DeliveryZoneKey>("inside_dhaka");
  const [estimate, setEstimate] = useState<Estimate | null>(null);
  const [failed, setFailed] = useState(false);

  // The remembered zone is read after mount (localStorage
  // is device state; the first paint uses the default).
  useEffect(() => {
    setZone(readStoredZone());
  }, []);

  useEffect(() => {
    let cancelled = false;
    setFailed(false);
    getDeliveryEstimate(zone, weightGrams)
      .then((data) => {
        if (!cancelled) setEstimate(data);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [zone, weightGrams]);

  function onZoneChange(next: DeliveryZoneKey) {
    setZone(next);
    window.localStorage.setItem(ZONE_STORAGE_KEY, next);
  }

  return (
    <section className="card delivery-estimate">
      <h2>ডেলিভারি — Delivery</h2>
      <label>
        আপনার এলাকা — Your area{" "}
        <select
          value={zone}
          onChange={(e) => onZoneChange(e.target.value as DeliveryZoneKey)}
        >
          {ZONES.map((z) => (
            <option key={z.key} value={z.key}>
              {z.label}
            </option>
          ))}
        </select>
      </label>
      {failed ? (
        <p>ডেলিভারির হিসাব লোড করা যায়নি — চেকআউটে সঠিক চার্জ দেখানো হবে।</p>
      ) : estimate === null ? (
        <p>হিসাব হচ্ছে…</p>
      ) : (
        <p>
          ডেলিভারি চার্জ <PriceTag amountPoisha={estimate.delivery_charge_poisha} />{" "}
          · পৌঁছাতে পারে{" "}
          {estimate.min_days === estimate.max_days
            ? `${estimate.min_days} দিনে`
            : `${estimate.min_days}–${estimate.max_days} দিনে`}{" "}
          — Delivery{" "}
          <PriceTag amountPoisha={estimate.delivery_charge_poisha} />, arrives
          in{" "}
          {estimate.min_days === estimate.max_days
            ? `${estimate.min_days} day`
            : `${estimate.min_days}–${estimate.max_days} days`}
          .
        </p>
      )}
      <p className="delivery-estimate-note">
        হিসাবটি এই পণ্যের একটির জন্য; কার্টের মোট ওজন অনুযায়ী চেকআউটে একই
        নিয়মে চার্জ ঠিক হবে। — For one unit of this product; checkout prices
        the whole cart by the same rule.
      </p>
    </section>
  );
}
