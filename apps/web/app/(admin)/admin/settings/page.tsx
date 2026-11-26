"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import {
  getAdminSettings,
  updateAdminSettings,
  type AdminSettings,
} from "@/lib/api";
import { formatBdt, parseTakaToPoisha } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

/**
 * Admin settings (Wave 10): the platform overrides. A stored
 * commission rate beats the startup COMMISSION_RATE_BPS at
 * checkout (existing orders keep the snapshot they were placed
 * with); a stored minimum payout beats the built-in 500-taka
 * floor for NEW payout requests. Wave 17 adds the COD advance
 * pair: the advance percent of the grand total, and the taka
 * threshold at/above which COD orders must collect it (0 turns
 * threshold-based advances off; risk-forced advances remain).
 * The form works in percent and taka for humans; the API speaks
 * bps and poisha.
 */
export default function AdminSettingsPage() {
  return (
    <AdminShell>
      <Settings />
    </AdminShell>
  );
}

function Settings() {
  const { token } = useAuth();
  const [settings, setSettings] = useState<AdminSettings | null>(null);
  const [commissionPct, setCommissionPct] = useState("");
  const [minPayoutTaka, setMinPayoutTaka] = useState("");
  const [advancePct, setAdvancePct] = useState("");
  const [advanceThresholdTaka, setAdvanceThresholdTaka] = useState("");
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const current = await getAdminSettings(token);
      setSettings(current);
      setCommissionPct(String(current.commission_bps / 100));
      setMinPayoutTaka(String(current.min_payout_poisha / 100));
      setAdvancePct(String(current.advance_percent_bps / 100));
      setAdvanceThresholdTaka(String(current.advance_threshold_poisha / 100));
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "সেটিংস লোড করা যায়নি।");
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onSave(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    const pct = Number(commissionPct);
    if (!Number.isFinite(pct) || pct < 0 || pct > 30) {
      setMessage("কমিশন ০% থেকে ৩০% এর মধ্যে হতে হবে।");
      return;
    }
    const minPoisha = parseTakaToPoisha(minPayoutTaka);
    if (minPoisha === null) {
      setMessage("সর্বনিম্ন পে-আউট একটি সঠিক টাকার পরিমাণ হতে হবে।");
      return;
    }
    const advPct = Number(advancePct);
    if (!Number.isFinite(advPct) || advPct < 0 || advPct > 100) {
      setMessage("অগ্রিমের হার ০% থেকে ১০০% এর মধ্যে হতে হবে।");
      return;
    }
    const thresholdPoisha = parseTakaToPoisha(advanceThresholdTaka);
    if (thresholdPoisha === null) {
      setMessage("অগ্রিমের থ্রেশহোল্ড একটি সঠিক টাকার পরিমাণ হতে হবে।");
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      const updated = await updateAdminSettings(token, {
        commission_bps: Math.round(pct * 100),
        min_payout_poisha: minPoisha,
        advance_percent_bps: Math.round(advPct * 100),
        advance_threshold_poisha: thresholdPoisha,
      });
      setSettings(updated);
      setMessage("সেটিংস সংরক্ষিত হয়েছে।");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "সেটিংস সংরক্ষণ হয়নি।");
    } finally {
      setBusy(false);
    }
  }

  if (!settings && !message) return <p>লোড হচ্ছে…</p>;

  return (
    <>
      <h2>সেটিংস — Settings</h2>
      {settings && (
        <p>
          প্ল্যাটফর্ম: {settings.platform_name} · বর্তমান কমিশন:{" "}
          {settings.commission_bps / 100}% · বর্তমান সর্বনিম্ন পে-আউট:{" "}
          {formatBdt(settings.min_payout_poisha)} · বর্তমান অগ্রিম:{" "}
          {settings.advance_percent_bps / 100}% (থ্রেশহোল্ড:{" "}
          {formatBdt(settings.advance_threshold_poisha)})
        </p>
      )}
      <form onSubmit={onSave}>
        <label>
          কমিশন (%)
          <input
            value={commissionPct}
            onChange={(e) => setCommissionPct(e.target.value)}
            inputMode="decimal"
          />
        </label>
        <label>
          সর্বনিম্ন পে-আউট (টাকা)
          <input
            value={minPayoutTaka}
            onChange={(e) => setMinPayoutTaka(e.target.value)}
            inputMode="decimal"
          />
        </label>
        <label>
          COD অগ্রিমের হার (%) — Advance percent
          <input
            value={advancePct}
            onChange={(e) => setAdvancePct(e.target.value)}
            inputMode="decimal"
          />
        </label>
        <label>
          COD অগ্রিমের থ্রেশহোল্ড (টাকা, ০ = বন্ধ) — Advance threshold
          <input
            value={advanceThresholdTaka}
            onChange={(e) => setAdvanceThresholdTaka(e.target.value)}
            inputMode="decimal"
          />
        </label>
        <button type="submit" disabled={busy}>
          সংরক্ষণ করুন
        </button>
      </form>
      {message && <p>{message}</p>}
    </>
  );
}
