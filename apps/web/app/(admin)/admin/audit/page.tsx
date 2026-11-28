"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import { getAdminAuditLog, type AdminAuditList } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

const PER_PAGE = 20;

/** The audited action vocabulary (Wave 28) — mirrors the API's
 * `audit` module doc; the filter sends one of these verbatim. */
const ACTION_OPTIONS = [
  "",
  "payout.approve",
  "payout.reject",
  "payout.mark_paid",
  "store.suspend",
  "store.reactivate",
  "settings.update",
  "verification.approve",
  "verification.reject",
  "verification.view_full_nid",
  "qna.hide",
  "qna.unhide",
  "return.refund_done",
  "coupon.create",
  "coupon.deactivate",
  "coupon.reactivate",
  "coupon.update_terms",
];

/**
 * Admin audit log (Wave 28): every consequential admin action
 * (payout decisions, store suspensions, settings changes,
 * verification decisions, Q&A moderation, refund completion),
 * written by the API inside the action's own transaction — so
 * a row here means the action really committed. The acting
 * admin is shown by masked phone only; details carry
 * non-secret context (changed setting KEYS, never values of
 * anything secret).
 */
export default function AdminAuditPage() {
  return (
    <AdminShell>
      <AuditLog />
    </AdminShell>
  );
}

function detailSummary(detail: Record<string, unknown> | null): string {
  if (!detail) return "—";
  const parts = Object.entries(detail).map(([key, value]) => {
    const rendered = Array.isArray(value)
      ? value.join(", ")
      : String(value);
    return `${key}: ${rendered}`;
  });
  return parts.length > 0 ? parts.join(" · ") : "—";
}

function AuditLog() {
  const { token } = useAuth();
  const [action, setAction] = useState("");
  const [page, setPage] = useState(1);
  const [list, setList] = useState<AdminAuditList | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      setList(
        await getAdminAuditLog(token, {
          page,
          perPage: PER_PAGE,
          action: action || undefined,
        })
      );
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "অডিট লগ লোড করা যায়নি।"
      );
    }
  }, [token, page, action]);

  useEffect(() => {
    void load();
  }, [load]);

  if (message) return <p>{message}</p>;
  if (!list) return <p>লোড হচ্ছে…</p>;

  const totalPages = Math.max(1, Math.ceil(list.total / list.limit));

  return (
    <>
      <h2>অডিট লগ — Audit log</h2>
      <p>
        <small>
          প্রতিটি সারি একটি সম্পন্ন অ্যাডমিন কাজ — পে-আউট সিদ্ধান্ত,
          স্টোর স্থগিত, সেটিংস পরিবর্তন, যাচাইকরণ, প্রশ্ন মডারেশন,
          রিফান্ড সম্পন্ন। লগটি কাজের নিজের ট্রানজ্যাকশনেই লেখা হয়,
          তাই এখানে থাকা মানেই কাজটি সত্যিই হয়েছে। বিবরণে কখনোই
          গোপন মান থাকে না।
        </small>
      </p>
      <p>
        অ্যাকশন:{" "}
        <select
          value={action}
          onChange={(event) => {
            setAction(event.target.value);
            setPage(1);
          }}
        >
          {ACTION_OPTIONS.map((option) => (
            <option key={option} value={option}>
              {option === "" ? "সব" : option}
            </option>
          ))}
        </select>
      </p>

      {list.items.length === 0 ? (
        <p>কোনো অডিট এন্ট্রি নেই।</p>
      ) : (
        <table className="inventory-table">
          <thead>
            <tr>
              <th>সময়</th>
              <th>অ্যাডমিন</th>
              <th>অ্যাকশন</th>
              <th>টার্গেট</th>
              <th>বিবরণ</th>
            </tr>
          </thead>
          <tbody>
            {list.items.map((entry) => (
              <tr key={entry.id}>
                <td>{new Date(entry.created_at).toLocaleString("en-GB")}</td>
                <td>{entry.admin_phone_masked}</td>
                <td>{entry.action}</td>
                <td>
                  {entry.target_type}
                  {entry.target_id ? (
                    <>
                      {" "}
                      <small>{entry.target_id.slice(0, 8)}…</small>
                    </>
                  ) : null}
                </td>
                <td>{detailSummary(entry.detail)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <p>
        মোট {list.total}টি এন্ট্রি · পৃষ্ঠা {list.page}/{totalPages}{" "}
        <button
          type="button"
          disabled={page <= 1}
          onClick={() => setPage((current) => Math.max(1, current - 1))}
        >
          আগের
        </button>{" "}
        <button
          type="button"
          disabled={page >= totalPages}
          onClick={() =>
            setPage((current) => Math.min(totalPages, current + 1))
          }
        >
          পরের
        </button>
      </p>
    </>
  );
}
