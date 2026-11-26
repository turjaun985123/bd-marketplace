"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import {
  approveReturnRequest,
  listSellerReturnRequests,
  markReturnReceived,
  refundStatusLabel,
  rejectReturnRequest,
  returnStatusLabel,
  type SellerReturnRequest,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Seller returns inbox (Wave 16): every return request for the
 * seller's stores, newest first, with the decision actions the
 * API allows from each state — approve / reject while
 * `requested`, mark received once `approved` (the goods came
 * back). Receiving is the money step: the API snapshots the
 * refund (goods minus coupon discount, delivery never refunded),
 * opens it as pending, marks the sub-order returned and restocks
 * the items in one transaction. Illegal moves come back as 409
 * and are shown as-is; the list reloads after every action so
 * what is shown is the server's state.
 */
export default function SellerReturnsPage() {
  const { ready, token } = useAuth();
  const [returns, setReturns] = useState<SellerReturnRequest[] | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [notes, setNotes] = useState<Record<string, string>>({});

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listSellerReturnRequests(token, { limit: 50 });
      setReturns(list.items);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "রিটার্ন লোড করা যায়নি।"
      );
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  async function act(
    ret: SellerReturnRequest,
    action: "approve" | "reject" | "received"
  ) {
    if (!token) return;
    setBusyId(ret.id);
    setMessage(null);
    try {
      if (action === "approve") {
        await approveReturnRequest(token, ret.id, notes[ret.id] || undefined);
      } else if (action === "reject") {
        await rejectReturnRequest(token, ret.id, notes[ret.id] || undefined);
      } else {
        await markReturnReceived(token, ret.id);
      }
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "অ্যাকশন সম্পন্ন হয়নি।");
    } finally {
      setBusyId(null);
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
        <h1>রিটার্ন — Returns</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>রিটার্ন — Returns</h1>
      <p>
        <Link href="/seller">ড্যাশবোর্ডে ফিরে যান — Back to dashboard</Link>
      </p>
      {message && <p role="status">{message}</p>}
      {returns === null ? (
        <p>লোড হচ্ছে…</p>
      ) : returns.length === 0 ? (
        <p>এখনো কোনো রিটার্নের আবেদন নেই। (No return requests yet.)</p>
      ) : (
        returns.map((ret) => (
          <article key={ret.id} className="card">
            <p>
              <strong>{returnStatusLabel(ret.status)}</strong> ({ret.status})
              · {new Date(ret.created_at).toLocaleString("bn-BD")} · ক্রেতা:{" "}
              {ret.buyer_phone_masked}
            </p>
            {ret.items.map((item, idx) => (
              <p key={idx}>
                {item.title_snapshot} × {item.quantity} —{" "}
                <PriceTag amountPoisha={item.line_total_poisha} />
              </p>
            ))}
            <p>কারণ: {ret.reason}</p>
            {ret.seller_note && <p>আপনার নোট: {ret.seller_note}</p>}
            <p>
              রিফান্ড: {refundStatusLabel(ret.refund_status)}
              {ret.refund_poisha !== null && (
                <>
                  {" "}
                  — <PriceTag amountPoisha={ret.refund_poisha} />
                </>
              )}
            </p>
            {ret.status === "requested" && (
              <p>
                <input
                  value={notes[ret.id] ?? ""}
                  onChange={(e) =>
                    setNotes((prev) => ({ ...prev, [ret.id]: e.target.value }))
                  }
                  placeholder="নোট (ঐচ্ছিক)"
                />{" "}
                <button
                  type="button"
                  disabled={busyId === ret.id}
                  onClick={() => void act(ret, "approve")}
                >
                  অনুমোদন করুন
                </button>{" "}
                <button
                  type="button"
                  disabled={busyId === ret.id}
                  onClick={() => void act(ret, "reject")}
                >
                  প্রত্যাখ্যান করুন
                </button>
              </p>
            )}
            {ret.status === "approved" && (
              <p>
                <button
                  type="button"
                  disabled={busyId === ret.id}
                  onClick={() => void act(ret, "received")}
                >
                  পণ্য ফেরত পেয়েছি — Mark received
                </button>{" "}
                পণ্য হাতে পেলে চাপুন: স্টক ফিরে যাবে, রিফান্ড পেন্ডিং হবে।
              </p>
            )}
          </article>
        ))
      )}
    </main>
  );
}
