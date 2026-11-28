"use client";

import { useState, type FormEvent } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import { formatDateTime } from "@/lib/format";
import { checkGiftCardBalance, type GiftCardBalance } from "@/lib/api";

/**
 * Gift-card balance check (Wave 60): the public front
 * door of the bearer instrument. Anyone holding a code
 * can see its balance, its store and its state — that is
 * what a gift card is for. Unknown codes get the API's
 * one uniform "not found" answer, so this page cannot be
 * used to guess codes; the code's entropy is the
 * protection, and only its hash is ever stored.
 *
 * A card spends only in its own store, at checkout — the
 * result links the holder to that storefront.
 */

const STATUS_TEXT: Record<string, string> = {
  active: "কার্ডটি সক্রিয় — খরচ করা যাবে। (Active — ready to spend.)",
  disabled:
    "কার্ডটি দোকান থেকে বন্ধ করা আছে। (This card has been disabled by its store.)",
  expired: "কার্ডটির মেয়াদ শেষ হয়ে গেছে। (This card has expired.)",
  depleted: "কার্ডের ব্যালেন্স শেষ। (This card has no balance left.)",
};

export default function GiftCardBalancePage() {
  const [code, setCode] = useState("");
  const [result, setResult] = useState<GiftCardBalance | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);

  async function onCheck(event: FormEvent) {
    event.preventDefault();
    if (!code.trim()) return;
    setChecking(true);
    setError(null);
    setResult(null);
    try {
      setResult(await checkGiftCardBalance(code.trim()));
    } catch (err) {
      setError(
        err instanceof Error
          ? err.message
          : "গিফট কার্ড পাওয়া যায়নি — gift card not found."
      );
    } finally {
      setChecking(false);
    }
  }

  return (
    <main>
      <h1>গিফট কার্ডের ব্যালেন্স — Gift card balance</h1>
      <p>
        কার্ডের কোডটি লিখুন — ব্যালেন্স, দোকান আর অবস্থা দেখা যাবে।
        কার্ডটি শুধু তার নিজের দোকানেই চেকআউটে খরচ করা যায়।
        (Enter the card code to see its balance, store and status.
        A card spends only in its own store, at checkout.)
      </p>
      <form onSubmit={(event) => void onCheck(event)}>
        <p>
          <label>
            কোড — Code
            <input
              value={code}
              onChange={(e) => setCode(e.target.value)}
              placeholder="bdgc_…"
              autoComplete="off"
            />
          </label>{" "}
          <button type="submit" disabled={checking || !code.trim()}>
            {checking ? "যাচাই হচ্ছে…" : "ব্যালেন্স দেখুন — Check"}
          </button>
        </p>
      </form>
      {error && <p role="alert">{error}</p>}
      {result && (
        <section>
          <p>
            দোকান:{" "}
            <Link href={`/${result.store_slug}`}>{result.store_name}</Link>{" "}
            · কার্ড ••{result.last4}
          </p>
          <p>
            ব্যালেন্স:{" "}
            <strong>
              <PriceTag amountPoisha={result.balance_poisha} />
            </strong>{" "}
            (মোট ছিল <PriceTag amountPoisha={result.initial_amount_poisha} />)
          </p>
          <p>{STATUS_TEXT[result.status] ?? result.status}</p>
          <p>
            {result.expires_at
              ? `মেয়াদ: ${formatDateTime(result.expires_at)}`
              : "এই কার্ডের কোনো মেয়াদ নেই — this card never expires."}
          </p>
          {result.status === "active" && (
            <p>
              <Link href={`/${result.store_slug}`}>
                {result.store_name} থেকে কিনুন — Shop at {result.store_name}
              </Link>
            </p>
          )}
        </section>
      )}
    </main>
  );
}
