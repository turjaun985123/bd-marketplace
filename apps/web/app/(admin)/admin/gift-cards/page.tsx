"use client";

import { useCallback, useEffect, useState, type FormEvent } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import { formatDateTime, parseTakaToPoisha } from "@/lib/format";
import {
  adjustAdminGiftCard,
  disableAdminGiftCard,
  enableAdminGiftCard,
  getAdminGiftCard,
  listAdminGiftCards,
  type GiftCard,
  type GiftCardDetail,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Admin gift cards (Wave 60): the support surface for
 * store-scoped stored value. An admin can find any store's
 * card (by code prefix, last4, or store slug), read its
 * ledger, freeze or unfreeze it, and correct its balance
 * with a mandatory explanation. Every one of those writes
 * is audited on the API in the same transaction as the
 * change — this page only surfaces them.
 *
 * Codes and hashes never appear here: the list and detail
 * carry the same masked fragments the seller sees. A lost
 * code cannot be recovered by anyone, admin included —
 * the recovery path is an audited balance adjustment onto
 * a NEW card, never a reveal.
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

export default function AdminGiftCardsPage() {
  const { ready, token } = useAuth();
  const [cards, setCards] = useState<GiftCard[]>([]);
  const [total, setTotal] = useState(0);
  const [query, setQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState("");
  const [message, setMessage] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [detail, setDetail] = useState<GiftCardDetail | null>(null);
  const [loadingDetailId, setLoadingDetailId] = useState<string | null>(null);

  // Adjust form state (per selected card).
  const [adjustTaka, setAdjustTaka] = useState("");
  const [adjustDirection, setAdjustDirection] = useState<"add" | "remove">(
    "add"
  );
  const [adjustNote, setAdjustNote] = useState("");
  const [adjusting, setAdjusting] = useState(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listAdminGiftCards(token, {
        q: query.trim() || undefined,
        status: statusFilter || undefined,
        limit: 100,
      });
      setCards(list.items);
      setTotal(list.total);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "গিফট কার্ড লোড করা যায়নি।"
      );
    }
  }, [token, query, statusFilter]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onToggleDisabled(card: GiftCard) {
    if (!token) return;
    setBusyId(card.id);
    setMessage(null);
    try {
      if (card.disabled) {
        await enableAdminGiftCard(token, card.id);
      } else {
        await disableAdminGiftCard(token, card.id);
      }
      await load();
      if (detail && detail.id === card.id) {
        setDetail(await getAdminGiftCard(token, card.id));
      }
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "কার্ডের অবস্থা বদলানো যায়নি।"
      );
    } finally {
      setBusyId(null);
    }
  }

  async function onShowDetail(card: GiftCard) {
    if (!token) return;
    if (detail && detail.id === card.id) {
      setDetail(null);
      return;
    }
    setLoadingDetailId(card.id);
    try {
      setDetail(await getAdminGiftCard(token, card.id));
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "কার্ডের বিবরণ লোড করা যায়নি।"
      );
    } finally {
      setLoadingDetailId(null);
    }
  }

  async function onAdjust(event: FormEvent) {
    event.preventDefault();
    if (!token || !detail) return;
    const amount = parseTakaToPoisha(adjustTaka);
    if (amount === null || amount <= 0) {
      setMessage("সংশোধনের পরিমাণ সঠিকভাবে লিখুন — enter the adjustment amount in taka.");
      return;
    }
    if (!adjustNote.trim()) {
      setMessage("সংশোধনের কারণ আবশ্যক — an adjustment note is required.");
      return;
    }
    setAdjusting(true);
    setMessage(null);
    try {
      const delta = adjustDirection === "add" ? amount : -amount;
      await adjustAdminGiftCard(token, detail.id, delta, adjustNote.trim());
      setAdjustTaka("");
      setAdjustNote("");
      setDetail(await getAdminGiftCard(token, detail.id));
      await load();
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "ব্যালেন্স সংশোধন করা যায়নি।"
      );
    } finally {
      setAdjusting(false);
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
      <h1>গিফট কার্ড — Gift cards (admin)</h1>
      <p>
        সব স্টোরের কার্ড — কোড প্রিফিক্স, শেষ ৪ সংখ্যা বা স্টোরের
        স্লাগ দিয়ে খুঁজুন। কোড বা হ্যাশ এখানে কখনো দেখা যায় না;
        হারানো কোড উদ্ধার করা যায় না — সমাধান হলো নতুন কার্ডে
        নিরীক্ষিত ব্যালেন্স সংশোধন। (Search by code prefix, last4
        or store slug. Codes and hashes are never shown; a lost
        code cannot be recovered — the remedy is an audited
        adjustment onto a new card.)
      </p>

      {message && <p role="alert">{message}</p>}

      <p>
        <label>
          খুঁজুন — Search{" "}
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="bdgc_… / 5N1D / store-slug"
          />
        </label>{" "}
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
        </label>{" "}
        · মোট: {total}
      </p>

      {cards.length === 0 ? (
        <p>কোনো কার্ড মেলেনি — No cards found.</p>
      ) : (
        <ul>
          {cards.map((card) => (
            <li key={card.id}>
              <code>{card.code_prefix}…{card.last4}</code> · {card.store_name}{" "}
              ({card.store_slug}) · {STATUS_LABELS[card.status] ?? card.status}{" "}
              · ব্যালেন্স: <PriceTag amountPoisha={card.balance_poisha} /> /{" "}
              <PriceTag amountPoisha={card.initial_amount_poisha} />
              {card.expires_at
                ? ` · মেয়াদ: ${formatDateTime(card.expires_at)}`
                : " · মেয়াদহীন"}{" "}
              <button
                type="button"
                disabled={loadingDetailId === card.id}
                onClick={() => void onShowDetail(card)}
              >
                {detail && detail.id === card.id
                  ? "বিবরণ লুকান"
                  : "বিবরণ — Detail"}
              </button>{" "}
              <button
                type="button"
                disabled={busyId === card.id}
                onClick={() => void onToggleDisabled(card)}
              >
                {card.disabled ? "চালু করুন — Enable" : "বন্ধ করুন — Disable"}
              </button>
              {detail && detail.id === card.id && (
                <>
                  <ul>
                    {detail.ledger.map((entry) => (
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
                    ))}
                  </ul>
                  <form onSubmit={(event) => void onAdjust(event)}>
                    <p>
                      <strong>ব্যালেন্স সংশোধন — Adjust balance</strong>{" "}
                      (নিরীক্ষিত — audited)
                    </p>
                    <p>
                      <label>
                        পরিমাণ (টাকা)
                        <input
                          value={adjustTaka}
                          onChange={(e) => setAdjustTaka(e.target.value)}
                          inputMode="decimal"
                          placeholder="100"
                        />
                      </label>{" "}
                      <label>
                        <select
                          value={adjustDirection}
                          onChange={(e) =>
                            setAdjustDirection(
                              e.target.value === "remove" ? "remove" : "add"
                            )
                          }
                        >
                          <option value="add">যোগ — Add</option>
                          <option value="remove">বিয়োগ — Remove</option>
                        </select>
                      </label>{" "}
                      <label>
                        কারণ (আবশ্যক — reason, required)
                        <input
                          value={adjustNote}
                          onChange={(e) => setAdjustNote(e.target.value)}
                        />
                      </label>{" "}
                      <button type="submit" disabled={adjusting}>
                        {adjusting ? "হচ্ছে…" : "সংশোধন করুন — Adjust"}
                      </button>
                    </p>
                  </form>
                </>
              )}
            </li>
          ))}
        </ul>
      )}
    </main>
  );
}
