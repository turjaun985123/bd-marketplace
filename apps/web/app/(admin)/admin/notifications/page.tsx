"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import {
  getAdminNotifications,
  type AdminNotificationList,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

const STATUS_OPTIONS = ["", "pending", "sending", "sent", "failed"];

/**
 * Admin notifications view (Wave 29): the SMS outbox the API
 * funnels write to (order placed, sub-order shipped /
 * delivered / cancelled / returned, payout paid, return
 * updates) and the worker drains. The counts are the queue's
 * health at a glance — above all `failed`: rows that used up
 * their 3 send attempts. Without a real SMS gateway every
 * row lands there with `sms_not_configured` as the error —
 * visible here, never silently dropped. Phone numbers are
 * masked by the API.
 */
export default function AdminNotificationsPage() {
  return (
    <AdminShell>
      <Notifications />
    </AdminShell>
  );
}

function Notifications() {
  const { token } = useAuth();
  const [status, setStatus] = useState("");
  const [list, setList] = useState<AdminNotificationList | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      setList(
        await getAdminNotifications(token, {
          status: status || undefined,
          limit: 50,
        })
      );
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "নোটিফিকেশন লোড করা যায়নি।"
      );
    }
  }, [token, status]);

  useEffect(() => {
    void load();
  }, [load]);

  if (message) return <p>{message}</p>;
  if (!list) return <p>লোড হচ্ছে…</p>;

  const { counts, items } = list;

  return (
    <>
      <h2>নোটিফিকেশন — Notifications (SMS outbox)</h2>
      <p>
        <small>
          প্রতিটি সারি একটি কিউ করা SMS — অর্ডার, ডেলিভারি,
          পে-আউট বা রিটার্নের ঘটনা ঘটার ট্রানজ্যাকশনেই লেখা
          হয়, ওয়ার্কার সেগুলো পাঠায়। সর্বোচ্চ ৩ বার চেষ্টার
          পর ব্যর্থ হলে সারিটি এখানে `failed` হয়ে থেকে যায় —
          কখনোই নীরবে হারিয়ে যায় না। আসল SMS গেটওয়ে না
          থাকলে সব সারিই `sms_not_configured` ত্রুটি নিয়ে
          ব্যর্থ হবে — এটাই প্রত্যাশিত চিত্র।
        </small>
      </p>

      <div className="card">
        <p>
          অপেক্ষমাণ (pending): <strong>{counts.pending}</strong>
          {" · "}পাঠানো হচ্ছে (sending): <strong>{counts.sending}</strong>
          {" · "}পাঠানো হয়েছে (sent): <strong>{counts.sent}</strong>
          {" · "}ব্যর্থ (failed): <strong>{counts.failed}</strong>
        </p>
      </div>

      <p>
        অবস্থা:{" "}
        <select
          value={status}
          onChange={(event) => setStatus(event.target.value)}
        >
          {STATUS_OPTIONS.map((option) => (
            <option key={option} value={option}>
              {option === "" ? "সব" : option}
            </option>
          ))}
        </select>{" "}
        <button type="button" onClick={() => void load()}>
          আবার লোড
        </button>
      </p>

      {items.length === 0 ? (
        <p>কোনো নোটিফিকেশন নেই।</p>
      ) : (
        <table className="inventory-table">
          <thead>
            <tr>
              <th>সময়</th>
              <th>ধরন</th>
              <th>ফোন</th>
              <th>অবস্থা</th>
              <th>চেষ্টা</th>
              <th>শেষ ত্রুটি</th>
              <th>পাঠানো হয়েছে</th>
            </tr>
          </thead>
          <tbody>
            {items.map((item) => (
              <tr key={item.id}>
                <td>{new Date(item.created_at).toLocaleString("en-GB")}</td>
                <td>{item.kind}</td>
                <td>{item.phone_masked}</td>
                <td>{item.status}</td>
                <td>{item.attempts}</td>
                <td>{item.last_error ?? "—"}</td>
                <td>
                  {item.sent_at
                    ? new Date(item.sent_at).toLocaleString("en-GB")
                    : "—"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  );
}
