"use client";

import { useCallback, useEffect, useState } from "react";
import PriceTag from "@/components/PriceTag";
import {
  adjustVariantStock,
  createVariant,
  deactivateVariant,
  listSellerVariants,
  updateVariant,
  type SellerVariant,
} from "@/lib/api";
import { parseTakaToPoisha } from "@/lib/format";

/** The seller stock-adjustment reasons (same vocabulary as the inventory page). */
const REASON_OPTIONS: { value: string; label: string }[] = [
  { value: "restock", label: "নতুন স্টক এসেছে — Restock" },
  { value: "correction", label: "সংশোধন — Correction" },
  { value: "damage", label: "নষ্ট / ক্ষতি — Damage" },
  { value: "count_fix", label: "গণনা মিলানো — Count fix" },
  { value: "other", label: "অন্যান্য — Other" },
];

/**
 * Parse a signed taka difference ("-50", "25.50") into poisha.
 * An empty field means a zero difference (a same-price
 * variant). Returns null for anything unparseable — the
 * caller shows a message instead of guessing.
 */
function parseDeltaTaka(input: string): number | null {
  const trimmed = input.trim();
  if (trimmed === "") return 0;
  const negative = trimmed.startsWith("-");
  const parsed = parseTakaToPoisha(negative ? trimmed.slice(1) : trimmed);
  if (parsed === null) return null;
  return negative ? -parsed : parsed;
}

/**
 * Seller variant manager (Wave 33): the variants of ONE
 * product — list with stock and final price, active toggle,
 * add form (name / SKU / price difference in taka / starting
 * stock), and per-variant audited stock adjustments.
 *
 * The rules live on the API: the final price (base + delta)
 * must stay positive, stock movements are audited with a
 * reason, and deactivation is the only delete. This
 * component only collects the numbers and shows results.
 * While a product has active variants its stock lives on
 * them — the product-level stock controls stop applying,
 * which the inventory page's API error also states.
 */
export default function VariantManager({
  token,
  productId,
}: {
  token: string;
  productId: string;
}) {
  const [variants, setVariants] = useState<SellerVariant[] | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  // Add-form state (delta typed in taka for humans).
  const [name, setName] = useState("");
  const [sku, setSku] = useState("");
  const [deltaTaka, setDeltaTaka] = useState("");
  const [stock, setStock] = useState("0");
  const [creating, setCreating] = useState(false);

  const load = useCallback(async () => {
    try {
      setVariants(await listSellerVariants(token, productId));
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "ভ্যারিয়েন্ট লোড করা যায়নি।"
      );
    }
  }, [token, productId]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onCreate(e: React.FormEvent) {
    e.preventDefault();
    const delta = parseDeltaTaka(deltaTaka);
    if (delta === null) {
      setMessage("দামের পার্থক্য হিসেবে একটি সংখ্যা লিখুন (যেমন 50 বা -50)।");
      return;
    }
    const stockValue = Number(stock);
    if (!Number.isInteger(stockValue) || stockValue < 0) {
      setMessage("স্টক হিসেবে ০ বা তার বেশি একটি পূর্ণ সংখ্যা লিখুন।");
      return;
    }
    setCreating(true);
    setMessage(null);
    try {
      await createVariant(token, productId, {
        name: name.trim(),
        sku: sku.trim() || undefined,
        price_delta_poisha: delta,
        stock: stockValue,
      });
      setName("");
      setSku("");
      setDeltaTaka("");
      setStock("0");
      await load();
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "ভ্যারিয়েন্ট যোগ করা যায়নি।"
      );
    } finally {
      setCreating(false);
    }
  }

  async function onToggleActive(variant: SellerVariant) {
    setMessage(null);
    try {
      if (variant.is_active) {
        await deactivateVariant(token, productId, variant.id);
      } else {
        await updateVariant(token, productId, variant.id, { is_active: true });
      }
      await load();
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "ভ্যারিয়েন্ট বদলানো যায়নি।"
      );
    }
  }

  return (
    <div className="card">
      <h4>ভ্যারিয়েন্ট — Variants (আকার/রং)</h4>
      {message && <p role="alert">{message}</p>}
      {variants === null ? (
        <p>লোড হচ্ছে…</p>
      ) : (
        <>
          {variants.length === 0 ? (
            <p>এই পণ্যের কোনো ভ্যারিয়েন্ট নেই।</p>
          ) : (
            variants.map((variant) => (
              <VariantRow
                key={variant.id}
                token={token}
                productId={productId}
                variant={variant}
                onChanged={() => void load()}
                onError={setMessage}
                onToggleActive={() => void onToggleActive(variant)}
              />
            ))
          )}
          <form onSubmit={(e) => void onCreate(e)}>
            <label>
              ভ্যারিয়েন্টের নাম (যেমন XL, লাল)
              <input
                value={name}
                onChange={(e) => setName(e.target.value)}
                maxLength={80}
                required
              />
            </label>{" "}
            <label>
              SKU (ঐচ্ছিক)
              <input value={sku} onChange={(e) => setSku(e.target.value)} />
            </label>{" "}
            <label>
              দামের পার্থক্য (টাকা, কম হলে -50)
              <input
                value={deltaTaka}
                onChange={(e) => setDeltaTaka(e.target.value)}
                inputMode="text"
                placeholder="0"
              />
            </label>{" "}
            <label>
              শুরুর স্টক
              <input
                value={stock}
                onChange={(e) => setStock(e.target.value)}
                inputMode="numeric"
                required
              />
            </label>{" "}
            <button type="submit" disabled={creating}>
              {creating ? "যোগ হচ্ছে…" : "ভ্যারিয়েন্ট যোগ করুন"}
            </button>
          </form>
        </>
      )}
    </div>
  );
}

/**
 * One variant row: its price maths and stock, the active
 * toggle, and the inline audited stock adjustment (the
 * Wave 23 inventory pattern, per variant).
 */
function VariantRow({
  token,
  productId,
  variant,
  onChanged,
  onError,
  onToggleActive,
}: {
  token: string;
  productId: string;
  variant: SellerVariant;
  onChanged: () => void;
  onError: (message: string | null) => void;
  onToggleActive: () => void;
}) {
  const [delta, setDelta] = useState("");
  const [reason, setReason] = useState("restock");
  const [busy, setBusy] = useState(false);

  async function onAdjust(e: React.FormEvent) {
    e.preventDefault();
    const amount = Number(delta);
    if (!Number.isInteger(amount) || amount === 0) {
      onError("স্টকের পরিমাণ হিসেবে শূন্য ছাড়া একটি পূর্ণ সংখ্যা লিখুন।");
      return;
    }
    setBusy(true);
    onError(null);
    try {
      await adjustVariantStock(token, productId, variant.id, {
        delta: amount,
        reason,
      });
      setDelta("");
      onChanged();
    } catch (err) {
      onError(err instanceof Error ? err.message : "স্টক বদলানো যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div>
      <p>
        <strong>{variant.name}</strong>
        {variant.sku && <> · SKU: {variant.sku}</>} · দাম{" "}
        <PriceTag amountPoisha={variant.final_price_poisha} /> (পার্থক্য{" "}
        <PriceTag amountPoisha={variant.price_delta_poisha} />) · স্টক{" "}
        {variant.stock} · {variant.is_active ? "সক্রিয়" : "বন্ধ"}{" "}
        <button type="button" onClick={onToggleActive}>
          {variant.is_active ? "বন্ধ করুন" : "চালু করুন"}
        </button>
      </p>
      <form onSubmit={(e) => void onAdjust(e)}>
        <input
          value={delta}
          onChange={(e) => setDelta(e.target.value)}
          placeholder="+৫ / −২"
          size={6}
          inputMode="numeric"
          aria-label="স্টক বদল"
        />{" "}
        <select value={reason} onChange={(e) => setReason(e.target.value)}>
          {REASON_OPTIONS.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>{" "}
        <button type="submit" disabled={busy}>
          {busy ? "হচ্ছে…" : "স্টক প্রয়োগ করুন"}
        </button>
      </form>
    </div>
  );
}
