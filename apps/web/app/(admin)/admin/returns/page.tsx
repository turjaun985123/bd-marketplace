"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import PriceTag from "@/components/PriceTag";
import {
  listAdminReturnRequests,
  markRefundDone,
  refundStatusLabel,
  returnStatusLabel,
  type SellerReturnRequest,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

const REFUND_FILTERS: { value: string; label: string }[] = [
  { value: "", label: "সব" },
  { value: "pending", label: "রিফান্ড বাকি" },
  { value: "done", label: "রিফান্ড দেওয়া হয়েছে" },
  { value: "none", label: "রিফান্ড প্রযোজ্য নয়" },
];

/**
 * Admin refund queue (Wave 16): every return request on the
 * platform, filterable by refund status. A refund becomes
 * `pending` when the seller receives the returned goods; the
 * platform then refunds the buyer OFF-platform (bKash / cash —
 * no gateway refund API is wired), and "রিফান্ড সম্পন্ন" here
 * RECORDS that fact. Marking done works only from pending — a
 * second attempt is a 409, so a refund can never be recorded
 * twice.
 */
export default function AdminReturnsPage() {
  return (
    <AdminShell>
      <Returns />
    </AdminShell>
  );
}

function Returns() {
  const { token } = useAuth();
  const [returns, setReturns] = useState<SellerReturnRequest[]>([]);
  const [refundStatus, setRefundStatus] = useState("pending");
  const [message, setMessage] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listAdminReturnRequests(token, {
        refund_status: refundStatus || undefined,
        limit: 50,
      });
      setReturns(list.items);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "রিটার্ন লোড করা যায়নি।"
      );
    }
  }, [token, refundStatus]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onMarkDone(ret: SellerReturnRequest) {
    if (!token) return;
    setBusyId(ret.id);
    setMessage(null);
    try {
      await markRefundDone(token, ret.id);
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "চিহ্নিত করা যায়নি।");
    } finally {
      setBusyId(null);
    }
  }

  return (
    <>
      <h2>রিটার্ন ও রিফান্ড — Returns & refunds</h2>
      <label>
        রিফান্ড স্ট্যাটাস
        <select
          value={refundStatus}
          onChange={(e) => setRefundStatus(e.target.value)}
        >
          {REFUND_FILTERS.map((f) => (
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
            <th>ক্রেতা</th>
            <th>কারণ</th>
            <th>রিটার্ন</th>
            <th>রিফান্ড</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {returns.map((ret) => (
            <tr key={ret.id}>
              <td>{ret.store_slug}</td>
              <td>{ret.buyer_phone_masked}</td>
              <td>
                {ret.reason}
                {ret.seller_note && (
                  <>
                    <br />
                    <small>বিক্রেতার নোট: {ret.seller_note}</small>
                  </>
                )}
              </td>
              <td>{returnStatusLabel(ret.status)}</td>
              <td>
                {refundStatusLabel(ret.refund_status)}
                {ret.refund_poisha !== null && (
                  <>
                    <br />
                    <PriceTag amountPoisha={ret.refund_poisha} />
                  </>
                )}
              </td>
              <td>
                {ret.refund_status === "pending" && (
                  <button
                    type="button"
                    disabled={busyId === ret.id}
                    onClick={() => void onMarkDone(ret)}
                  >
                    রিফান্ড সম্পন্ন
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {returns.length === 0 && <p>এই ফিল্টারে কোনো রিটার্ন নেই।</p>}
    </>
  );
}
