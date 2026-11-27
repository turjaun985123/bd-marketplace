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
 * Wave 21 adds the KYC payout gate checkbox: while it is on, a
 * store must hold at least the basic verification tier to
 * request a payout. Wave 26 adds the per-day sponsored-
 * promotion price: a stored value reprices NEW promotions
 * only — bought promotions keep their cost snapshot. The form
 * works in percent and taka for humans; the API speaks bps
 * and poisha.
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
  const [kycGate, setKycGate] = useState(false);
  const [promotionPriceTaka, setPromotionPriceTaka] = useState("");
  const [mediaMaxImages, setMediaMaxImages] = useState("");
  const [mediaQuotaMb, setMediaQuotaMb] = useState("");
  // Wave 54: the per-zone delivery-time ranges, as six
  // day fields (earliest/latest per zone).
  const [daysInsideMin, setDaysInsideMin] = useState("");
  const [daysInsideMax, setDaysInsideMax] = useState("");
  const [daysSuburbMin, setDaysSuburbMin] = useState("");
  const [daysSuburbMax, setDaysSuburbMax] = useState("");
  const [daysOutsideMin, setDaysOutsideMin] = useState("");
  const [daysOutsideMax, setDaysOutsideMax] = useState("");
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
      setKycGate(current.kyc_payout_gate);
      setPromotionPriceTaka(
        String(current.promotion_price_per_day_poisha / 100)
      );
      setMediaMaxImages(String(current.media_max_images_per_product));
      setMediaQuotaMb(String(current.media_store_quota_mb));
      setDaysInsideMin(String(current.delivery_days_inside_dhaka[0]));
      setDaysInsideMax(String(current.delivery_days_inside_dhaka[1]));
      setDaysSuburbMin(String(current.delivery_days_dhaka_suburb[0]));
      setDaysSuburbMax(String(current.delivery_days_dhaka_suburb[1]));
      setDaysOutsideMin(String(current.delivery_days_outside_dhaka[0]));
      setDaysOutsideMax(String(current.delivery_days_outside_dhaka[1]));
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
    const promotionPoisha = parseTakaToPoisha(promotionPriceTaka);
    if (promotionPoisha === null || promotionPoisha <= 0) {
      setMessage("প্রমোশনের দৈনিক দাম একটি সঠিক ধনাত্মক টাকার পরিমাণ হতে হবে।");
      return;
    }
    const maxImages = Number(mediaMaxImages);
    if (!Number.isInteger(maxImages) || maxImages < 1 || maxImages > 50) {
      setMessage("পণ্যপ্রতি ছবির সীমা ১ থেকে ৫০ এর মধ্যে হতে হবে।");
      return;
    }
    const quotaMb = Number(mediaQuotaMb);
    if (!Number.isInteger(quotaMb) || quotaMb < 10 || quotaMb > 102400) {
      setMessage("স্টোরের মিডিয়া কোটা ১০ থেকে ১০২৪০০ MB এর মধ্যে হতে হবে।");
      return;
    }
    // Wave 54: each zone's range must be whole days,
    // 0–30, earliest not after latest (the API enforces
    // the same rule; this is the form's early answer).
    const dayRanges: [string, string, string][] = [
      [daysInsideMin, daysInsideMax, "ঢাকার ভিতরে"],
      [daysSuburbMin, daysSuburbMax, "ঢাকার উপশহর"],
      [daysOutsideMin, daysOutsideMax, "ঢাকার বাইরে"],
    ];
    const parsedRanges: [number, number][] = [];
    for (const [minRaw, maxRaw, label] of dayRanges) {
      const min = Number(minRaw);
      const max = Number(maxRaw);
      if (
        !Number.isInteger(min) ||
        !Number.isInteger(max) ||
        min < 0 ||
        max > 30 ||
        min > max
      ) {
        setMessage(
          `${label}: ডেলিভারির সময় ০–৩০ দিনের মধ্যে হতে হবে এবং শুরুর দিন শেষের দিনের পরে হতে পারবে না।`
        );
        return;
      }
      parsedRanges.push([min, max]);
    }
    setBusy(true);
    setMessage(null);
    try {
      const updated = await updateAdminSettings(token, {
        commission_bps: Math.round(pct * 100),
        min_payout_poisha: minPoisha,
        advance_percent_bps: Math.round(advPct * 100),
        advance_threshold_poisha: thresholdPoisha,
        kyc_payout_gate: kycGate,
        promotion_price_per_day_poisha: promotionPoisha,
        media_max_images_per_product: maxImages,
        media_store_quota_mb: quotaMb,
        delivery_days_inside_dhaka: parsedRanges[0],
        delivery_days_dhaka_suburb: parsedRanges[1],
        delivery_days_outside_dhaka: parsedRanges[2],
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
          {formatBdt(settings.advance_threshold_poisha)}) · KYC পে-আউট
          গেট: {settings.kyc_payout_gate ? "চালু" : "বন্ধ"} · প্রমোশনের
          দৈনিক দাম: {formatBdt(settings.promotion_price_per_day_poisha)}
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
        <label>
          <input
            type="checkbox"
            checked={kycGate}
            onChange={(e) => setKycGate(e.target.checked)}
          />{" "}
          KYC পে-আউট গেট চালু — পে-আউট পেতে স্টোর যাচাইকরণ (কমপক্ষে
          basic স্তর) আবশ্যক
        </label>
        <label>
          প্রমোশনের দাম (প্রতি দিন, টাকা) — Promotion price per day
          <input
            value={promotionPriceTaka}
            onChange={(e) => setPromotionPriceTaka(e.target.value)}
            inputMode="decimal"
          />
        </label>
        <label>
          পণ্যপ্রতি সর্বোচ্চ ছবি (১–৫০) — Max images per product
          <input
            value={mediaMaxImages}
            onChange={(e) => setMediaMaxImages(e.target.value)}
            inputMode="numeric"
          />
        </label>
        <label>
          স্টোরপ্রতি মিডিয়া কোটা (MB) — Media quota per store
          <input
            value={mediaQuotaMb}
            onChange={(e) => setMediaQuotaMb(e.target.value)}
            inputMode="numeric"
          />
        </label>
        <fieldset>
          <legend>
            ডেলিভারির আনুমানিক সময় (দিন) — Delivery time estimates (Wave 54)
          </legend>
          <p>
            পণ্যের পেজে ক্রেতাকে দেখানো সময়সীমা — সবচেয়ে আগে ও সবচেয়ে
            দেরিতে পৌঁছানোর দিন। চার্জের নিয়ম এতে বদলায় না।
          </p>
          <label>
            ঢাকার ভিতরে — Inside Dhaka: থেকে (earliest)
            <input
              value={daysInsideMin}
              onChange={(e) => setDaysInsideMin(e.target.value)}
              inputMode="numeric"
            />
          </label>
          <label>
            পর্যন্ত (latest)
            <input
              value={daysInsideMax}
              onChange={(e) => setDaysInsideMax(e.target.value)}
              inputMode="numeric"
            />
          </label>
          <label>
            ঢাকার উপশহর — Dhaka suburb: থেকে (earliest)
            <input
              value={daysSuburbMin}
              onChange={(e) => setDaysSuburbMin(e.target.value)}
              inputMode="numeric"
            />
          </label>
          <label>
            পর্যন্ত (latest)
            <input
              value={daysSuburbMax}
              onChange={(e) => setDaysSuburbMax(e.target.value)}
              inputMode="numeric"
            />
          </label>
          <label>
            ঢাকার বাইরে — Outside Dhaka: থেকে (earliest)
            <input
              value={daysOutsideMin}
              onChange={(e) => setDaysOutsideMin(e.target.value)}
              inputMode="numeric"
            />
          </label>
          <label>
            পর্যন্ত (latest)
            <input
              value={daysOutsideMax}
              onChange={(e) => setDaysOutsideMax(e.target.value)}
              inputMode="numeric"
            />
          </label>
        </fieldset>
        <button type="submit" disabled={busy}>
          সংরক্ষণ করুন
        </button>
      </form>
      {message && <p>{message}</p>}
    </>
  );
}
