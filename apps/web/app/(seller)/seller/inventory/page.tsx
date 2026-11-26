"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  adjustStock,
  getSellerInventory,
  getStockHistory,
  setLowStockThreshold,
  stockReasonLabel,
  stockStatusLabel,
  type Inventory,
  type InventoryItem,
  type StockAdjustment,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

const REASON_OPTIONS: { value: string; label: string }[] = [
  { value: "restock", label: "নতুন স্টক এসেছে — Restock" },
  { value: "correction", label: "সংশোধন — Correction" },
  { value: "damage", label: "নষ্ট / ক্ষতি — Damage" },
  { value: "count_fix", label: "গণনা মিলানো — Count fix" },
  { value: "other", label: "অন্যান্য — Other" },
];

/**
 * Seller inventory (Wave 23): every product of the seller's
 * stores with its live stock health, manual stock adjustments,
 * per-product low-stock thresholds, and the audited movement
 * history behind each product.
 *
 * The audit rule lives on the API: every adjustment made here
 * is recorded with its reason (and so are the system movements
 * from orders, cancels and returns), and a move that would
 * drive stock below zero is refused there. This page only
 * collects the numbers and shows the results.
 */
export default function SellerInventoryPage() {
  const { ready, token } = useAuth();
  const [inventory, setInventory] = useState<Inventory | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      setInventory(await getSellerInventory(token));
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "ইনভেন্টরি লোড করা যায়নি।"
      );
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  if (ready && !token) {
    return (
      <main>
        <h1>ইনভেন্টরি</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  const summary = inventory?.summary;

  return (
    <main>
      <h1>ইনভেন্টরি — Inventory</h1>
      <p>
        <Link href="/seller">← ড্যাশবোর্ডে ফিরুন — Back to dashboard</Link>
      </p>
      {message && <p role="status">{message}</p>}
      {inventory === null ? (
        <p>লোড হচ্ছে…</p>
      ) : (
        <>
          <section>
            <div className="card">
              <p>
                মোট পণ্য: <strong>{summary?.products ?? 0}</strong>
                {" · "}
                স্টক শেষ: <strong>{summary?.out_of_stock ?? 0}</strong>
                {" · "}
                কম স্টক: <strong>{summary?.low_stock ?? 0}</strong>
              </p>
            </div>
          </section>
          {inventory.items.length === 0 ? (
            <p>এখনো কোনো পণ্য নেই। (No products yet.)</p>
          ) : (
            <table className="inventory-table">
              <thead>
                <tr>
                  <th>পণ্য</th>
                  <th>স্টক</th>
                  <th>অবস্থা</th>
                  <th>সতর্কতার সীমা</th>
                  <th>স্টক বদলান</th>
                </tr>
              </thead>
              <tbody>
                {inventory.items.map((item) => (
                  <InventoryRow
                    key={item.product_id}
                    token={token!}
                    item={item}
                    onChanged={() => void load()}
                    onError={setMessage}
                  />
                ))}
              </tbody>
            </table>
          )}
        </>
      )}
    </main>
  );
}

/**
 * One inventory row: the product's health, its threshold
 * editor, the adjustment form, and an expandable history of
 * every audited movement (fetched on first open).
 */
function InventoryRow({
  token,
  item,
  onChanged,
  onError,
}: {
  token: string;
  item: InventoryItem;
  onChanged: () => void;
  onError: (message: string | null) => void;
}) {
  const [delta, setDelta] = useState("");
  const [reason, setReason] = useState("restock");
  const [note, setNote] = useState("");
  const [adjustBusy, setAdjustBusy] = useState(false);
  const [threshold, setThreshold] = useState(
    item.low_stock_threshold === null ? "" : String(item.low_stock_threshold)
  );
  const [thresholdBusy, setThresholdBusy] = useState(false);
  const [history, setHistory] = useState<StockAdjustment[] | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);

  async function onAdjust(e: React.FormEvent) {
    e.preventDefault();
    const amount = Number(delta);
    if (!Number.isInteger(amount) || amount === 0) {
      onError("স্টকের পরিমাণ হিসেবে শূন্য ছাড়া একটি পূর্ণ সংখ্যা লিখুন।");
      return;
    }
    setAdjustBusy(true);
    onError(null);
    try {
      await adjustStock(token, item.product_id, {
        delta: amount,
        reason,
        note: note.trim() || undefined,
      });
      setDelta("");
      setNote("");
      setHistory(null);
      onChanged();
    } catch (err) {
      onError(err instanceof Error ? err.message : "স্টক বদলানো যায়নি।");
    } finally {
      setAdjustBusy(false);
    }
  }

  async function onSaveThreshold(e: React.FormEvent) {
    e.preventDefault();
    const trimmed = threshold.trim();
    const value = trimmed === "" ? null : Number(trimmed);
    if (value !== null && (!Number.isInteger(value) || value < 0)) {
      onError("সীমা হিসেবে ০ বা তার বেশি একটি পূর্ণ সংখ্যা লিখুন, অথবা খালি রাখুন।");
      return;
    }
    setThresholdBusy(true);
    onError(null);
    try {
      await setLowStockThreshold(token, item.product_id, value);
      onChanged();
    } catch (err) {
      onError(err instanceof Error ? err.message : "সীমা সেভ করা যায়নি।");
    } finally {
      setThresholdBusy(false);
    }
  }

  async function onToggleHistory() {
    if (!historyOpen && history === null) {
      try {
        const page = await getStockHistory(token, item.product_id, {
          per_page: 50,
        });
        setHistory(page.items);
      } catch (err) {
        onError(err instanceof Error ? err.message : "ইতিহাস লোড করা যায়নি।");
        return;
      }
    }
    setHistoryOpen((open) => !open);
  }

  return (
    <>
      <tr>
        <td>
          <strong>{item.title}</strong>
          <br />
          <small>{item.store_slug}</small>
          <br />
          <button type="button" onClick={() => void onToggleHistory()}>
            {historyOpen ? "ইতিহাস লুকান" : "স্টকের ইতিহাস — History"}
          </button>
        </td>
        <td>{item.stock}</td>
        <td>
          <span className={`stock-badge stock-badge-${item.stock_status}`}>
            {stockStatusLabel(item.stock_status)}
          </span>
        </td>
        <td>
          <form onSubmit={(e) => void onSaveThreshold(e)}>
            <input
              value={threshold}
              onChange={(e) => setThreshold(e.target.value)}
              placeholder="ডিফল্ট (৫)"
              size={8}
              inputMode="numeric"
            />{" "}
            <button type="submit" disabled={thresholdBusy}>
              {thresholdBusy ? "সেভ হচ্ছে…" : "সেভ"}
            </button>
          </form>
          <small>কার্যকর সীমা: {item.effective_threshold}</small>
        </td>
        <td>
          <form onSubmit={(e) => void onAdjust(e)}>
            <input
              value={delta}
              onChange={(e) => setDelta(e.target.value)}
              placeholder="+৫ / −২"
              size={6}
              inputMode="numeric"
            />{" "}
            <select value={reason} onChange={(e) => setReason(e.target.value)}>
              {REASON_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>{" "}
            <input
              value={note}
              onChange={(e) => setNote(e.target.value)}
              placeholder="নোট (ঐচ্ছিক)"
              size={14}
            />{" "}
            <button type="submit" disabled={adjustBusy}>
              {adjustBusy ? "হচ্ছে…" : "প্রয়োগ করুন"}
            </button>
          </form>
        </td>
      </tr>
      {historyOpen && (
        <tr>
          <td colSpan={5}>
            {history === null ? (
              <p>লোড হচ্ছে…</p>
            ) : history.length === 0 ? (
              <p>এই পণ্যের স্টকে এখনো কোনো নথিভুক্ত বদল হয়নি।</p>
            ) : (
              <ul>
                {history.map((row) => (
                  <li key={row.id}>
                    {new Date(row.created_at).toLocaleString("bn-BD")} —{" "}
                    {row.delta > 0 ? `+${row.delta}` : row.delta} (
                    {stockReasonLabel(row.reason)}
                    {row.actor === "system" ? ", সিস্টেম" : ""}) → স্টক{" "}
                    {row.new_stock}
                    {row.note ? ` — ${row.note}` : ""}
                  </li>
                ))}
              </ul>
            )}
          </td>
        </tr>
      )}
    </>
  );
}
