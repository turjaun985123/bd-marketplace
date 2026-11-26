"use client";

import { useState } from "react";
import PriceTag from "./PriceTag";
import { useDeliveryCharge, type DeliveryZone } from "@/hooks/useDeliveryCharge";

/** Demo widget for Wave 1: quotes a delivery charge from the Rust API. */
export default function DeliveryChargeWidget() {
  const [zone, setZone] = useState<DeliveryZone>("InsideDhaka");
  const [weightKg, setWeightKg] = useState(1);
  const { chargePoisha, loading, offlineMessage, quote } = useDeliveryCharge();

  return (
    <section className="card">
      <h2>ডেলিভারি চার্জ দেখুন — Delivery charge demo</h2>
      <label htmlFor="zone">এলাকা (Zone)</label>
      <select
        id="zone"
        value={zone}
        onChange={(e) => setZone(e.target.value as DeliveryZone)}
      >
        <option value="InsideDhaka">ঢাকার ভিতরে</option>
        <option value="DhakaSuburb">ঢাকার উপশহর</option>
        <option value="OutsideDhaka">ঢাকার বাইরে</option>
      </select>
      <label htmlFor="weight">ওজন (কেজি)</label>
      <input
        id="weight"
        type="number"
        min={1}
        value={weightKg}
        onChange={(e) => setWeightKg(Math.max(1, Number(e.target.value) || 1))}
      />
      <p>
        <button disabled={loading} onClick={() => quote(zone, weightKg)}>
          {loading ? "হিসাব হচ্ছে…" : "চার্জ হিসাব করুন"}
        </button>
      </p>
      {chargePoisha !== null && (
        <p>
          ডেলিভারি চার্জ: <PriceTag amountPoisha={chargePoisha} />
        </p>
      )}
      {offlineMessage && <p role="alert">{offlineMessage}</p>}
    </section>
  );
}
