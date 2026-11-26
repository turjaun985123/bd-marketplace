"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import PriceTag from "@/components/PriceTag";
import {
  approvePayout,
  listAdminPayouts,
  markPayoutPaid,
  payoutStatusLabel,
  rejectPayout,
  type AdminPayout,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

const STATUS_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব" },
  { value: "requested", label: "অনুরোধ করা হয়েছে" },
  { value: "approved", label: "অনুমোদিত" },
  { value: "paid", label: "পরিশোধিত" },
  { value: "rejected", label: "প্রত্যাখ্যাত" },
];

/**
 * Admin payout queue (Wave 10): the processing side of the Wave 9
 * payout lifecycle. requested -> approved -> paid, or requested
 * -> rejected with a reason; the API's transition table rejects
 * every other move with a 409 (shown as-is). Rejecting releases
 * the amount back into the seller's available balance.
 */
export default function AdminPayoutsPage() {
  return (
    <AdminShell>
      <Payouts />
    </AdminShell>
  );
}

function Payouts() {
  const { token } = useAuth();
  const [payouts, setPayouts] = useState<AdminPayout[]>([]);
  const [status, setStatus] = useState("requested");
  const [message, setMessage] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listAdminPayouts(token, {
        status: status || undefined,
        limit: 50,
      });
      setPayouts(list.items);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "পে-আউট লোড করা যায়নি।");
    }
  }, [token, status]);

  useEffect(() => {
    void load();
  }, [load]);

  async function act(payout: AdminPayout, action: "approve" | "reject" | "paid") {
    if (!token) return;
    setBusyId(payout.id);
    setMessage(null);
    try {
      if (action === "approve") await approvePayout(token, payout.id);
      if (action === "paid") await markPayoutPaid(token, payout.id);
      if (action === "reject") {
        const note = window.prompt("প্রত্যাখ্যানের কারণ (নোট):") ?? undefined;
        await rejectPayout(token, payout.id, note);
      }
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "অ্যাকশন সম্পন্ন হয়নি।");
    } finally {
      setBusyId(null);
    }
  }

  return (
    <>
      <h2>পে-আউট — Payouts</h2>
      <label>
        স্ট্যাটাস
        <select value={status} onChange={(e) => setStatus(e.target.value)}>
          {STATUS_FILTERS.map((f) => (
            <option key={f.value} value={f.value}>
              {f.label}
            </option>
          ))}
        </select>
      </label>
      {message && <p>{message}</p>}
      <table>
        <thead>
          <tr>
            <th>স্টোর</th>
            <th>পরিমাণ</th>
            <th>অ্যাকাউন্ট</th>
            <th>স্ট্যাটাস</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {payouts.map((payout) => (
            <tr key={payout.id}>
              <td>
                {payout.store_name} <small>({payout.owner_phone})</small>
              </td>
              <td>
                <PriceTag amountPoisha={payout.amount_poisha} />
              </td>
              <td>
                {payout.method} — {payout.account_number}
                <br />
                <small>{payout.account_name}</small>
                {payout.note && (
                  <>
                    <br />
                    <small>নোট: {payout.note}</small>
                  </>
                )}
              </td>
              <td>{payoutStatusLabel(payout.status)}</td>
              <td>
                {payout.status === "requested" && (
                  <>
                    <button
                      type="button"
                      disabled={busyId === payout.id}
                      onClick={() => void act(payout, "approve")}
                    >
                      অনুমোদন
                    </button>{" "}
                    <button
                      type="button"
                      disabled={busyId === payout.id}
                      onClick={() => void act(payout, "reject")}
                    >
                      প্রত্যাখ্যান
                    </button>
                  </>
                )}
                {payout.status === "approved" && (
                  <button
                    type="button"
                    disabled={busyId === payout.id}
                    onClick={() => void act(payout, "paid")}
                  >
                    পরিশোধিত চিহ্নিত করুন
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {payouts.length === 0 && <p>এই স্ট্যাটাসে কোনো পে-আউট নেই।</p>}
    </>
  );
}
