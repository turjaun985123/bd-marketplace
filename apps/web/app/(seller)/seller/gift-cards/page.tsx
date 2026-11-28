"use client";

import { useCallback, useEffect, useState, type FormEvent } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import { formatDateTime, parseTakaToPoisha } from "@/lib/format";
import {
  disableGiftCard,
  enableGiftCard,
  getGiftCard,
  getGiftCardStats,
  issueGiftCards,
  listGiftCards,
  type CreatedGiftCard,
  type GiftCard,
  type GiftCardDetail,
  type GiftCardStats,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Gift cards (Wave 60): the seller's store-scoped stored
 * value. Issuance here is PROMOTIONAL — no money is
 * collected when a card is created, so when a buyer spends
 * one, the redemption comes out of the seller's own side
 * of that order (commission and payout are computed net of
 * it, like a store coupon). The page says so plainly,
 * because a seller minting cards is minting their own
 * future discounts.
 *
 * The full code exists ONLY in the issue response: this
 * page holds freshly issued codes in a reveal panel until
 * dismissed, with a copy button; the list below carries
 * prefixes and last-4s only, and no reload can bring a
 * code back. Issuing and freezing are owner-level actions
 * on the API (managers may view this page but their issue
 * or freeze attempts answer 403, surfaced as the message).
 */

const STATUS_LABELS: Record<string, string> = {
  active: "সক্রিয় — Active",
  disabled: "বন্ধ — Disabled",
  expired: "মেয়াদোত্তীর্ণ — Expired",
  depleted: "শেষ — Depleted",
};

const LEDGER_LABELS: Record<string, string> = {
  issue: "ইস্যু — Issued",
  redeem: "খরচ — Redeemed",
  cancel_restore: "বাতিলে ফেরত — Cancel restore",
  return_restore: "রিটার্নে ফেরত — Return restore",
  admin_adjust: "অ্যাডমিন সংশোধন — Admin adjust",
};

export default function SellerGiftCardsPage() {
  const { ready, token } = useAuth();
  const [cards, setCards] = useState<GiftCard[]>([]);
  const [stats, setStats] = useState<GiftCardStats | null>(null);
  const [statusFilter, setStatusFilter] = useState("");
  const [message, setMessage] = useState<string | null>(null);
  const [revealed, setRevealed] = useState<CreatedGiftCard[]>([]);
  const [copiedCode, setCopiedCode] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [detail, setDetail] = useState<GiftCardDetail | null>(null);
  const [loadingDetailId, setLoadingDetailId] = useState<string | null>(null);

  // Issue form state. The amount is typed in TAKA and
  // converted with the shared parser (poisha never typed
  // by hand).
  const [amountTaka, setAmountTaka] = useState("");
  const [note, setNote] = useState("");
  const [recipient, setRecipient] = useState("");
  const [validityDays, setValidityDays] = useState("");
  const [noExpiry, setNoExpiry] = useState(false);
  const [count, setCount] = useState("1");
  const [issuing, setIssuing] = useState(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const [list, totals] = await Promise.all([
        listGiftCards(token, {
          status: statusFilter || undefined,
          limit: 100,
        }),
        getGiftCardStats(token),
      ]);
      setCards(list.items);
      setStats(totals);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "গিফট কার্ড লোড করা যায়নি।"
      );
    }
  }, [token, statusFilter]);

  useEffect(() => {
    void load();
  }, [load]);

  // Cards whose value lapses within a week (Wave 60
  // extension): derived from the loaded list, mirroring
  // the worker sweep's window — active, balance-holding,
  // expiry inside 7 days. Only computed on the unfiltered
  // list, so a status filter never hides the warning by
  // accident and never fabricates one either.
  const weekFromNow = Date.now() + 7 * 24 * 60 * 60 * 1000;
  const expiringSoon =
    statusFilter === ""
      ? cards.filter(
          (card) =>
            card.status === "active" &&
            card.balance_poisha > 0 &&
            card.expires_at !== null &&
            new Date(card.expires_at).getTime() <= weekFromNow
        )
      : [];

  async function onIssue(event: FormEvent) {
    event.preventDefault();
    if (!token) return;
    const amountPoisha = parseTakaToPoisha(amountTaka);
    if (amountPoisha === null) {
      setMessage("টাকার পরিমাণ সঠিকভাবে লিখুন — enter the amount in taka.");
      return;
    }
    const days = validityDays.trim() === "" ? undefined : Number(validityDays);
    if (!noExpiry && days !== undefined && (!Number.isInteger(days) || days < 1)) {
      setMessage("মেয়াদের দিন ১ বা তার বেশি হতে হবে — validity days must be a whole number ≥ 1.");
      return;
    }
    const parsedCount = Number(count);
    if (!Number.isInteger(parsedCount) || parsedCount < 1 || parsedCount > 50) {
      setMessage("কার্ডের সংখ্যা ১–৫০ এর মধ্যে হতে হবে — count must be 1–50.");
      return;
    }
    setIssuing(true);
    setMessage(null);
    setRevealed([]);
    setCopiedCode(null);
    try {
      const issued = await issueGiftCards(token, {
        amount_poisha: amountPoisha,
        note: note.trim() ? note.trim() : undefined,
        recipient_name: recipient.trim() ? recipient.trim() : undefined,
        validity_days: noExpiry ? undefined : days,
        no_expiry: noExpiry,
        count: parsedCount,
      });
      setRevealed(issued.cards);
      setAmountTaka("");
      setNote("");
      setRecipient("");
      setValidityDays("");
      setNoExpiry(false);
      setCount("1");
      await load();
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "গিফট কার্ড তৈরি করা যায়নি।"
      );
    } finally {
      setIssuing(false);
    }
  }

  async function onToggleDisabled(card: GiftCard) {
    if (!token) return;
    setBusyId(card.id);
    setMessage(null);
    try {
      if (card.disabled) {
        await enableGiftCard(token, card.id);
      } else {
        await disableGiftCard(token, card.id);
      }
      await load();
      if (detail && detail.id === card.id) {
        setDetail(await getGiftCard(token, card.id));
      }
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "কার্ডের অবস্থা বদলানো যায়নি।"
      );
    } finally {
      setBusyId(null);
    }
  }

  async function onShowLedger(card: GiftCard) {
    if (!token) return;
    if (detail && detail.id === card.id) {
      setDetail(null);
      return;
    }
    setLoadingDetailId(card.id);
    try {
      setDetail(await getGiftCard(token, card.id));
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "লেজার লোড করা যায়নি।"
      );
    } finally {
      setLoadingDetailId(null);
    }
  }

  async function onCopy(code: string) {
    try {
      await navigator.clipboard.writeText(code);
      setCopiedCode(code);
    } catch {
      setMessage("কপি করা যায়নি — কোডটি হাতে লিখে রাখুন। (Copy failed; note the code down.)");
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
        <h1>গিফট কার্ড — Gift cards</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>গিফট কার্ড — Gift cards</h1>
      <p>
        গিফট কার্ড শুধু আপনার স্টোরেই খরচ করা যাবে। এই কার্ডগুলো
        প্রচারণামূলক — তৈরির সময় কোনো টাকা নেওয়া হয় না, তাই
        ক্রেতা কার্ড খরচ করলে সেই ছাড় আপনার অর্ডারের হিসাব
        থেকেই কাটা যায় (কমিশন ও পেআউট ছাড়-পরবর্তী অঙ্কে হয়)।
        (Cards spend only in your store. Issuance is promotional —
        no money is collected up front, so a redemption is funded
        by you: commission and payout are computed net of it.)
      </p>

      {message && <p role="alert">{message}</p>}

      {stats && (
        <p>
          মোট কার্ড: {stats.cards_total} · সক্রিয়: {stats.active_cards} ·
          বকেয়া ব্যালেন্স: <PriceTag amountPoisha={stats.outstanding_poisha} />{" "}
          · মোট খরচ হয়েছে: <PriceTag amountPoisha={stats.redeemed_total_poisha} />{" "}
          · ফেরত এসেছে: <PriceTag amountPoisha={stats.restored_total_poisha} />
        </p>
      )}

      {expiringSoon.length > 0 && (
        <p role="alert">
          {expiringSoon.length}টি কার্ডের মেয়াদ ৭ দিনের মধ্যে শেষ হচ্ছে —
          বকেয়া ব্যালেন্স মোট{" "}
          <PriceTag
            amountPoisha={expiringSoon.reduce(
              (sum, card) => sum + card.balance_poisha,
              0
            )}
          />
          । প্রাপকদের আগেভাগে জানিয়ে দিন। (Cards expiring within 7
          days — the worker also sends you one SMS reminder per card.)
        </p>
      )}

      <h2>নতুন কার্ড তৈরি — Issue cards</h2>
      <form onSubmit={(event) => void onIssue(event)}>
        <p>
          <label>
            পরিমাণ (টাকা — amount in taka, ৳10–৳50,000)
            <input
              value={amountTaka}
              onChange={(e) => setAmountTaka(e.target.value)}
              inputMode="decimal"
              placeholder="500"
              required
            />
          </label>
        </p>
        <p>
          <label>
            প্রাপকের নাম (ঐচ্ছিক — recipient, optional)
            <input
              value={recipient}
              onChange={(e) => setRecipient(e.target.value)}
              maxLength={80}
            />
          </label>{" "}
          <label>
            নোট (ঐচ্ছিক — note, optional)
            <input
              value={note}
              onChange={(e) => setNote(e.target.value)}
              maxLength={200}
              placeholder="ঈদ মোবারক"
            />
          </label>
        </p>
        <p>
          <label>
            মেয়াদ (দিন — validity days; ফাঁকা = প্ল্যাটফর্মের ডিফল্ট)
            <input
              value={validityDays}
              onChange={(e) => setValidityDays(e.target.value)}
              inputMode="numeric"
              disabled={noExpiry}
              placeholder="365"
            />
          </label>{" "}
          <label>
            <input
              type="checkbox"
              checked={noExpiry}
              onChange={(e) => setNoExpiry(e.target.checked)}
            />{" "}
            মেয়াদহীন — Never expires
          </label>{" "}
          <label>
            সংখ্যা — Count (1–50)
            <input
              value={count}
              onChange={(e) => setCount(e.target.value)}
              inputMode="numeric"
            />
          </label>
        </p>
        <p>
          <button type="submit" disabled={issuing}>
            {issuing ? "তৈরি হচ্ছে…" : "কার্ড তৈরি করুন — Issue"}
          </button>
        </p>
      </form>

      {revealed.length > 0 && (
        <section>
          <h2>আপনার নতুন কোড — Your new code(s)</h2>
          <p>
            <strong>
              কোডগুলো শুধু এই একবারই দেখা যাবে — এখনই সংরক্ষণ করুন।
              (Each code is shown only once — save it now. Anyone
              holding a code can spend it.)
            </strong>
          </p>
          <ul>
            {revealed.map((card) => (
              <li key={card.id}>
                <code>{card.code}</code>{" "}
                <button type="button" onClick={() => void onCopy(card.code)}>
                  {copiedCode === card.code ? "কপি হয়েছে ✓" : "কপি — Copy"}
                </button>{" "}
                · <PriceTag amountPoisha={card.initial_amount_poisha} />
                {card.recipient_name ? ` · ${card.recipient_name}` : ""}
              </li>
            ))}
          </ul>
          <p>
            <button type="button" onClick={() => setRevealed([])}>
              লুকিয়ে ফেলুন — Dismiss
            </button>
          </p>
        </section>
      )}

      <h2>কার্ডের তালিকা — Cards</h2>
      <p>
        <label>
          অবস্থা — Status{" "}
          <select
            value={statusFilter}
            onChange={(e) => setStatusFilter(e.target.value)}
          >
            <option value="">সব — All</option>
            <option value="active">সক্রিয় — Active</option>
            <option value="disabled">বন্ধ — Disabled</option>
            <option value="expired">মেয়াদোত্তীর্ণ — Expired</option>
            <option value="depleted">শেষ — Depleted</option>
          </select>
        </label>
      </p>
      {cards.length === 0 ? (
        <p>কোনো কার্ড নেই — No cards yet.</p>
      ) : (
        <ul>
          {cards.map((card) => (
            <li key={card.id}>
              <code>{card.code_prefix}…{card.last4}</code> ·{" "}
              {STATUS_LABELS[card.status] ?? card.status} · ব্যালেন্স:{" "}
              <PriceTag amountPoisha={card.balance_poisha} /> /{" "}
              <PriceTag amountPoisha={card.initial_amount_poisha} />
              {card.recipient_name ? ` · ${card.recipient_name}` : ""}
              {card.expires_at
                ? ` · মেয়াদ: ${formatDateTime(card.expires_at)}`
                : " · মেয়াদহীন"}
              {" "}
              <button
                type="button"
                disabled={loadingDetailId === card.id}
                onClick={() => void onShowLedger(card)}
              >
                {detail && detail.id === card.id
                  ? "লেজার লুকান"
                  : "লেজার — Ledger"}
              </button>{" "}
              <button
                type="button"
                disabled={busyId === card.id}
                onClick={() => void onToggleDisabled(card)}
              >
                {card.disabled ? "চালু করুন — Enable" : "বন্ধ করুন — Disable"}
              </button>
              {detail && detail.id === card.id && (
                <ul>
                  {detail.ledger.length === 0 ? (
                    <li>কোনো লেনদেন নেই — No movements.</li>
                  ) : (
                    detail.ledger.map((entry) => (
                      <li key={entry.id}>
                        {LEDGER_LABELS[entry.kind] ?? entry.kind}:{" "}
                        {entry.delta_poisha >= 0 ? "+" : "−"}
                        <PriceTag
                          amountPoisha={Math.abs(entry.delta_poisha)}
                        />{" "}
                        · ব্যালেন্স:{" "}
                        <PriceTag amountPoisha={entry.balance_after_poisha} />{" "}
                        · {formatDateTime(entry.created_at)}
                        {entry.note ? ` · ${entry.note}` : ""}
                      </li>
                    ))
                  )}
                </ul>
              )}
            </li>
          ))}
        </ul>
      )}
    </main>
  );
}
