"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import {
  approveVerification,
  getAdminVerificationFull,
  listAdminVerifications,
  rejectVerification,
  type AdminVerification,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

const STATUS_FILTERS: { value: string; label: string }[] = [
  { value: "pending", label: "পর্যালোচনা বাকি" },
  { value: "approved", label: "অনুমোদিত" },
  { value: "rejected", label: "প্রত্যাখ্যাত" },
  { value: "all", label: "সব" },
];

/**
 * Admin verification queue (Wave 21): sellers' KYC
 * applications, pending first. The queue list is masked —
 * owner phone and NID — because triage does not need the full
 * number. The full NID is fetched ONLY when a reviewer clicks
 * "সম্পূর্ণ NID দেখুন" on one row (that call is the single
 * audited place the number leaves the API), and the reviewer
 * compares it against the seller's document image.
 *
 * Approving at `verified` needs a trade license on the
 * application — the button is disabled without one, matching
 * the API's 400. Rejection REQUIRES a note: the seller sees it
 * and resubmits after fixing the problem.
 */
export default function AdminVerificationsPage() {
  return (
    <AdminShell>
      <Verifications />
    </AdminShell>
  );
}

function Verifications() {
  const { token } = useAuth();
  const [items, setItems] = useState<AdminVerification[]>([]);
  const [status, setStatus] = useState("pending");
  const [message, setMessage] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  /** store_id -> revealed full NID (only after an explicit click). */
  const [revealed, setRevealed] = useState<Record<string, string>>({});
  /** store_id -> rejection note being typed. */
  const [notes, setNotes] = useState<Record<string, string>>({});

  const load = useCallback(async () => {
    if (!token) return;
    try {
      setItems(await listAdminVerifications(token, status));
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "আবেদন লোড করা যায়নি।"
      );
    }
  }, [token, status]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onReveal(item: AdminVerification) {
    if (!token) return;
    setMessage(null);
    try {
      const full = await getAdminVerificationFull(token, item.store_id);
      setRevealed((prev) => ({ ...prev, [item.store_id]: full.nid_number }));
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "NID দেখা যায়নি।");
    }
  }

  async function onApprove(item: AdminVerification, tier: "basic" | "verified") {
    if (!token) return;
    setBusyId(item.store_id);
    setMessage(null);
    try {
      await approveVerification(token, item.store_id, tier);
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "অনুমোদন করা যায়নি।");
    } finally {
      setBusyId(null);
    }
  }

  async function onReject(item: AdminVerification) {
    if (!token) return;
    const note = (notes[item.store_id] ?? "").trim();
    if (!note) {
      setMessage("প্রত্যাখ্যানের কারণ (নোট) লিখতে হবে।");
      return;
    }
    setBusyId(item.store_id);
    setMessage(null);
    try {
      await rejectVerification(token, item.store_id, note);
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "প্রত্যাখ্যান করা যায়নি।");
    } finally {
      setBusyId(null);
    }
  }

  return (
    <>
      <h2>স্টোর যাচাইকরণ — Verification queue</h2>
      <label>
        অবস্থা
        <select value={status} onChange={(e) => setStatus(e.target.value)}>
          {STATUS_FILTERS.map((f) => (
            <option key={f.value} value={f.value}>
              {f.label}
            </option>
          ))}
        </select>
      </label>
      {message && <p role="alert">{message}</p>}
      {items.length === 0 ? (
        <p>এই অবস্থায় কোনো আবেদন নেই।</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>স্টোর</th>
              <th>মালিকের ফোন</th>
              <th>NID</th>
              <th>ট্রেড লাইসেন্স</th>
              <th>ডকুমেন্ট</th>
              <th>অবস্থা</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {items.map((item) => (
              <tr key={item.store_id}>
                <td>
                  {item.store_name}
                  <br />
                  <small>{item.store_slug}</small>
                </td>
                <td>{item.owner_phone_masked}</td>
                <td>
                  {revealed[item.store_id] ?? item.nid_masked}
                  {!revealed[item.store_id] && (
                    <>
                      {" "}
                      <button
                        type="button"
                        onClick={() => void onReveal(item)}
                      >
                        সম্পূর্ণ NID দেখুন
                      </button>
                    </>
                  )}
                </td>
                <td>{item.has_trade_license ? "আছে" : "নেই"}</td>
                <td>
                  {item.document_url ? (
                    <a
                      href={item.document_url}
                      target="_blank"
                      rel="noreferrer"
                    >
                      ছবি দেখুন
                    </a>
                  ) : (
                    "—"
                  )}
                </td>
                <td>
                  {item.status}
                  {item.reviewer_note && (
                    <>
                      <br />
                      <small>নোট: {item.reviewer_note}</small>
                    </>
                  )}
                </td>
                <td>
                  {item.status === "pending" && (
                    <>
                      <button
                        type="button"
                        disabled={busyId === item.store_id}
                        onClick={() => void onApprove(item, "basic")}
                      >
                        basic অনুমোদন
                      </button>{" "}
                      <button
                        type="button"
                        disabled={
                          busyId === item.store_id || !item.has_trade_license
                        }
                        title={
                          item.has_trade_license
                            ? undefined
                            : "verified স্তরের জন্য ট্রেড লাইসেন্স আবশ্যক"
                        }
                        onClick={() => void onApprove(item, "verified")}
                      >
                        verified অনুমোদন
                      </button>{" "}
                      <input
                        value={notes[item.store_id] ?? ""}
                        onChange={(e) =>
                          setNotes((prev) => ({
                            ...prev,
                            [item.store_id]: e.target.value,
                          }))
                        }
                        placeholder="প্রত্যাখ্যানের কারণ"
                      />{" "}
                      <button
                        type="button"
                        disabled={busyId === item.store_id}
                        onClick={() => void onReject(item)}
                      >
                        প্রত্যাখ্যান
                      </button>
                    </>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  );
}
