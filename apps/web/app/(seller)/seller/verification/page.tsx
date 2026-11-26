"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import VerificationBadge from "@/components/VerificationBadge";
import {
  getSellerVerification,
  submitSellerVerification,
  type SellerVerification,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/** Bangla label for an application status. */
const STATUS_LABELS: Record<string, string> = {
  none: "আবেদন করা হয়নি (Not applied)",
  pending: "পর্যালোচনাধীন (Under review)",
  approved: "অনুমোদিত (Approved)",
  rejected: "প্রত্যাখ্যাত (Rejected)",
};

/**
 * Seller verification (Wave 21): the seller's KYC page. It
 * shows the store's current tier and application status, the
 * reviewer's rejection note when there is one, and the
 * submission form: NID number (required), trade license
 * number and document image URL (optional — the license is
 * what unlocks the higher `verified` tier).
 *
 * The NID is shown back ONLY in masked form — that is all the
 * API ever returns, even to the seller who typed it. A rejected
 * seller edits the same form and resubmits; an approved store
 * sees its badge instead of the form.
 */
export default function SellerVerificationPage() {
  const { ready, token } = useAuth();
  const [view, setView] = useState<SellerVerification | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [nid, setNid] = useState("");
  const [tradeLicense, setTradeLicense] = useState("");
  const [documentUrl, setDocumentUrl] = useState("");

  const fillForm = useCallback((v: SellerVerification) => {
    setView(v);
    setTradeLicense(v.trade_license_number ?? "");
    setDocumentUrl(v.document_url ?? "");
  }, []);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getSellerVerification(token)
      .then((v) => {
        if (!cancelled) fillForm(v);
      })
      .catch((err) => {
        if (!cancelled) {
          setMessage(
            err instanceof Error ? err.message : "যাচাইকরণ লোড করা যায়নি।"
          );
        }
      });
    return () => {
      cancelled = true;
    };
  }, [token, fillForm]);

  async function onSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setBusy(true);
    setMessage(null);
    try {
      const saved = await submitSellerVerification(token, {
        nid_number: nid.trim(),
        trade_license_number: tradeLicense.trim() || undefined,
        document_url: documentUrl.trim() || undefined,
      });
      setNid("");
      fillForm(saved);
      setMessage(
        "আবেদন জমা হয়েছে — প্ল্যাটফর্ম দল যাচাই করে জানাবে। (Application submitted for review.)"
      );
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "জমা দেওয়া যায়নি।");
    } finally {
      setBusy(false);
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
        <h1>স্টোর যাচাইকরণ</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  const approved = view?.status === "approved";

  return (
    <main>
      <h1>স্টোর যাচাইকরণ — Store verification</h1>
      <p>
        <Link href="/seller">← ড্যাশবোর্ডে ফিরুন — Dashboard</Link>
      </p>
      {message && <p role="status">{message}</p>}
      {view === null ? (
        <p>লোড হচ্ছে…</p>
      ) : (
        <>
          <section className="card">
            <p>
              বর্তমান অবস্থা:{" "}
              <strong>
                {STATUS_LABELS[view.status] ?? view.status}
              </strong>
            </p>
            <p>
              যাচাইকরণের স্তর (tier): <strong>{view.tier}</strong>{" "}
              <VerificationBadge tier={view.tier} />
            </p>
            {view.nid_masked && <p>জমা দেওয়া NID: {view.nid_masked}</p>}
            {view.trade_license_number && (
              <p>ট্রেড লাইসেন্স: {view.trade_license_number}</p>
            )}
            {view.status === "rejected" && view.reviewer_note && (
              <p role="alert">
                প্রত্যাখ্যানের কারণ: {view.reviewer_note}
                <br />
                সমস্যাটি ঠিক করে আবার আবেদন করুন।
              </p>
            )}
            {view.status === "pending" && (
              <p>
                আবেদনটি পর্যালোচনাধীন আছে। নতুন করে জমা দিলে আগের
                আবেদনটিই বদলে যাবে।
              </p>
            )}
          </section>

          {!approved && (
            <form onSubmit={(e) => void onSubmit(e)}>
              <h2>
                {view.status === "rejected"
                  ? "আবার আবেদন করুন — Resubmit"
                  : "যাচাইয়ের আবেদন — Apply for verification"}
              </h2>
              <p>
                জাতীয় পরিচয়পত্রের (NID) নম্বর আবশ্যক। ট্রেড লাইসেন্স
                নম্বর দিলে <strong>যাচাইকৃত ব্যবসা (verified)</strong>{" "}
                স্তরের জন্য বিবেচনা করা হবে; শুধু NID দিয়ে{" "}
                <strong>যাচাইকৃত বিক্রেতা (basic)</strong> স্তর পাওয়া
                যায়। ডকুমেন্টের ছবির URL দিলে যাচাই দ্রুত হয়।
              </p>
              <label>
                NID নম্বর (১০, ১৩ বা ১৭ সংখ্যা)
                <input
                  value={nid}
                  onChange={(e) => setNid(e.target.value)}
                  placeholder="যেমন 1234567890123"
                  inputMode="numeric"
                  required
                />
              </label>
              <label>
                ট্রেড লাইসেন্স নম্বর (ঐচ্ছিক)
                <input
                  value={tradeLicense}
                  onChange={(e) => setTradeLicense(e.target.value)}
                  placeholder="যেমন TL-2024-12345"
                />
              </label>
              <label>
                ডকুমেন্টের ছবির URL (ঐচ্ছিক, https:// দিয়ে শুরু)
                <input
                  value={documentUrl}
                  onChange={(e) => setDocumentUrl(e.target.value)}
                  placeholder="https://example.com/nid.jpg"
                />
              </label>
              <p>
                <button type="submit" disabled={busy}>
                  {busy ? "জমা হচ্ছে…" : "আবেদন জমা দিন — Submit"}
                </button>
              </p>
            </form>
          )}
        </>
      )}
    </main>
  );
}
