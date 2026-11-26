"use client";

import { useCallback, useState } from "react";

export type DeliveryZone = "InsideDhaka" | "DhakaSuburb" | "OutsideDhaka";

export interface DeliveryChargeState {
  chargePoisha: number | null;
  loading: boolean;
  /** Set when the Rust API is unreachable — the UI must say so honestly. */
  offlineMessage: string | null;
  quote: (zone: DeliveryZone, weightKg: number, codAmountPoisha?: number) => Promise<void>;
  /**
   * Same call as `quote`, but returns the charge (or null when the API
   * is unreachable) instead of only storing it — for callers that need
   * several quotes at once, e.g. one per seller group at checkout.
   */
  quoteAmount: (
    zone: DeliveryZone,
    weightKg: number,
    codAmountPoisha?: number
  ) => Promise<number | null>;
}

/**
 * Shared hook: the ONLY place the web app calls POST /v1/delivery-charge.
 * Marketplace checkout, storefront checkout and the seller manual-order
 * page all reuse this hook instead of re-implementing the fetch.
 */
export function useDeliveryCharge(): DeliveryChargeState {
  const [chargePoisha, setChargePoisha] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [offlineMessage, setOfflineMessage] = useState<string | null>(null);

  const quoteAmount = useCallback(
    async (
      zone: DeliveryZone,
      weightKg: number,
      codAmountPoisha = 0
    ): Promise<number | null> => {
      const base = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
      try {
        const res = await fetch(`${base}/v1/delivery-charge`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            zone,
            weight_kg: weightKg,
            cod_amount_poisha: codAmountPoisha,
          }),
        });
        if (!res.ok) throw new Error(`API returned ${res.status}`);
        const data: { delivery_charge_poisha: number } = await res.json();
        return data.delivery_charge_poisha;
      } catch {
        return null;
      }
    },
    []
  );

  const quote = useCallback(
    async (zone: DeliveryZone, weightKg: number, codAmountPoisha = 0) => {
      setLoading(true);
      setOfflineMessage(null);
      const charge = await quoteAmount(zone, weightKg, codAmountPoisha);
      if (charge === null) {
        setChargePoisha(null);
        setOfflineMessage(
          "API-তে সংযোগ করা যাচ্ছে না — Rust API চালু আছে কিনা দেখুন। (Could not reach the API.)"
        );
      } else {
        setChargePoisha(charge);
      }
      setLoading(false);
    },
    [quoteAmount]
  );

  return { chargePoisha, loading, offlineMessage, quote, quoteAmount };
}
