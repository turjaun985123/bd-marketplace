"use client";

import { useCallback, useEffect, useState } from "react";
import PriceTag from "@/components/PriceTag";
import { parseTakaToPoisha } from "@/lib/format";
import type { Coupon, CreateCouponInput } from "@/lib/api";

/**
 * The shared coupon manager (Wave 14): create form + list with
 * active toggles, used by BOTH the seller page (store coupons)
 * and the admin page (platform coupons) — the pages differ only
 * in which API functions they pass in and whether delete exists.
 * Money is typed in taka and converted to poisha here; every rule
 * (code shape, percent range, caps) is enforced by the API.
 */
export interface CouponManagerApi {
  load: () => Promise<Coupon[]>;
  create: (input: CreateCouponInput) => Promise<Coupon>;
  setActive: (id: string, active: boolean) => Promise<Coupon>;
  /** Sellers may delete an unused coupon; admins deactivate instead. */
  remove?: (id: string) => Promise<void>;
}

export default function CouponManager({ api }: { api: CouponManagerApi }) {
  const [coupons, setCoupons] = useState<Coupon[]>([]);
  const [message, setMessage] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  // Create-form state. Money fields are taka strings for humans.
  const [code, setCode] = useState("");
  const [discountType, setDiscountType] = useState("percent");
  const [percentValue, setPercentValue] = useState("10");
  const [fixedTaka, setFixedTaka] = useState("");
  const [minOrderTaka, setMinOrderTaka] = useState("");
  const [capTaka, setCapTaka] = useState("");
  const [startsAt, setStartsAt] = useState("");
  const [endsAt, setEndsAt] = useState("");
  const [usageLimit, setUsageLimit] = useState("");
  const [creating, setCreating] = useState(false);

  const load = useCallback(async () => {
    try {
      setCoupons(await api.load());
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "কুপন লোড করা যায়নি।");
    }
  }, [api]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onCreate(e: React.FormEvent) {
    e.preventDefault();
    setMessage(null);

    let discountValue: number;
    if (discountType === "percent") {
      discountValue = Number(percentValue);
      if (!Number.isInteger(discountValue) || discountValue < 1 || discountValue > 90) {
        setMessage("শতাংশ ছাড় ১–৯০ এর মধ্যে হতে হবে। (Percent must be 1–90.)");
        return;
      }
    } else {
      const poisha = parseTakaToPoisha(fixedTaka);
      if (poisha === null || poisha < 100) {
        setMessage("নির্দিষ্ট ছাড় কমপক্ষে ১ টাকা হতে হবে। (Fixed discount must be at least ৳1.)");
        return;
      }
      discountValue = poisha;
    }
    const minOrder = minOrderTaka.trim() ? parseTakaToPoisha(minOrderTaka) : 0;
    if (minOrder === null) {
      setMessage("সর্বনিম্ন অর্ডারের পরিমাণ ঠিকমতো লিখুন।");
      return;
    }
    const cap = capTaka.trim() ? parseTakaToPoisha(capTaka) : null;
    if (capTaka.trim() && cap === null) {
      setMessage("সর্বোচ্চ ছাড়ের পরিমাণ ঠিকমতো লিখুন।");
      return;
    }
    const limit = usageLimit.trim() ? Number(usageLimit) : null;
    if (limit !== null && (!Number.isInteger(limit) || limit <= 0)) {
      setMessage("ব্যবহারের সীমা ধনাত্মক সংখ্যা হতে হবে।");
      return;
    }

    setCreating(true);
    try {
      const created = await api.create({
        code: code.trim(),
        discount_type: discountType,
        discount_value: discountValue,
        min_order_poisha: minOrder,
        max_discount_poisha: discountType === "percent" ? cap : null,
        starts_at: startsAt ? new Date(startsAt).toISOString() : null,
        ends_at: endsAt ? new Date(endsAt).toISOString() : null,
        usage_limit: limit,
      });
      setCoupons((prev) => [created, ...prev]);
      setCode("");
      setMessage("কুপন তৈরি হয়েছে। (Coupon created.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "কুপন তৈরি করা যায়নি।");
    } finally {
      setCreating(false);
    }
  }

  async function onToggle(coupon: Coupon) {
    setBusyId(coupon.id);
    setMessage(null);
    try {
      const updated = await api.setActive(coupon.id, !coupon.active);
      setCoupons((prev) => prev.map((c) => (c.id === updated.id ? updated : c)));
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "অবস্থা বদলানো যায়নি।");
    } finally {
      setBusyId(null);
    }
  }

  async function onRemove(coupon: Coupon) {
    if (!api.remove) return;
    setBusyId(coupon.id);
    setMessage(null);
    try {
      await api.remove(coupon.id);
      setCoupons((prev) => prev.filter((c) => c.id !== coupon.id));
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "কুপন মোছা যায়নি।"
      );
    } finally {
      setBusyId(null);
    }
  }

  return (
    <>
      {message && <p role="status">{message}</p>}

      <h2>নতুন কুপন — New coupon</h2>
      <form onSubmit={onCreate}>
        <label>
          কোড (Code)
          <input
            value={code}
            onChange={(e) => setCode(e.target.value)}
            placeholder="EID10"
            required
          />
        </label>
        <label>
          ছাড়ের ধরন
          <select
            value={discountType}
            onChange={(e) => setDiscountType(e.target.value)}
          >
            <option value="percent">শতাংশ (%)</option>
            <option value="fixed">নির্দিষ্ট টাকা</option>
          </select>
        </label>
        {discountType === "percent" ? (
          <label>
            শতাংশ (১–৯০)
            <input
              value={percentValue}
              onChange={(e) => setPercentValue(e.target.value)}
              inputMode="numeric"
              required
            />
          </label>
        ) : (
          <label>
            ছাড়ের পরিমাণ (টাকা)
            <input
              value={fixedTaka}
              onChange={(e) => setFixedTaka(e.target.value)}
              placeholder="100"
              inputMode="decimal"
              required
            />
          </label>
        )}
        <label>
          সর্বনিম্ন অর্ডার (টাকা, ঐচ্ছিক)
          <input
            value={minOrderTaka}
            onChange={(e) => setMinOrderTaka(e.target.value)}
            inputMode="decimal"
          />
        </label>
        {discountType === "percent" && (
          <label>
            সর্বোচ্চ ছাড় (টাকা, ঐচ্ছিক)
            <input
              value={capTaka}
              onChange={(e) => setCapTaka(e.target.value)}
              inputMode="decimal"
            />
          </label>
        )}
        <label>
          শুরুর সময় (ঐচ্ছিক)
          <input
            type="datetime-local"
            value={startsAt}
            onChange={(e) => setStartsAt(e.target.value)}
          />
        </label>
        <label>
          শেষের সময় (ঐচ্ছিক)
          <input
            type="datetime-local"
            value={endsAt}
            onChange={(e) => setEndsAt(e.target.value)}
          />
        </label>
        <label>
          মোট ব্যবহারের সীমা (ঐচ্ছিক)
          <input
            value={usageLimit}
            onChange={(e) => setUsageLimit(e.target.value)}
            inputMode="numeric"
          />
        </label>
        <p>
          <button type="submit" disabled={creating}>
            {creating ? "তৈরি হচ্ছে…" : "কুপন তৈরি করুন — Create coupon"}
          </button>
        </p>
      </form>

      <h2>কুপনের তালিকা — Coupons</h2>
      {coupons.length === 0 ? (
        <p>কোনো কুপন নেই।</p>
      ) : (
        coupons.map((coupon) => (
          <article key={coupon.id} className="card">
            <h3>
              {coupon.code}{" "}
              {coupon.active ? "" : "(বন্ধ আছে — inactive)"}
              {coupon.store_id ? " · স্টোর কুপন (store coupon)" : " · প্ল্যাটফর্ম কুপন"}
            </h3>
            {coupon.admin_deactivated_reason && (
              <p>
                প্ল্যাটফর্ম বন্ধ করেছে — কারণ: {coupon.admin_deactivated_reason}
              </p>
            )}
            <p>
              ছাড়:{" "}
              {coupon.discount_type === "percent" ? (
                `${coupon.discount_value}%`
              ) : (
                <PriceTag amountPoisha={coupon.discount_value} />
              )}
              {coupon.max_discount_poisha !== null && (
                <>
                  {" "}
                  · সর্বোচ্চ <PriceTag amountPoisha={coupon.max_discount_poisha} />
                </>
              )}
              {coupon.min_order_poisha > 0 && (
                <>
                  {" "}
                  · সর্বনিম্ন অর্ডার{" "}
                  <PriceTag amountPoisha={coupon.min_order_poisha} />
                </>
              )}
            </p>
            <p>
              ব্যবহৃত: {coupon.used_count}
              {coupon.usage_limit !== null && ` / ${coupon.usage_limit}`}
              {coupon.starts_at && ` · শুরু: ${coupon.starts_at.slice(0, 16).replace("T", " ")}`}
              {coupon.ends_at && ` · শেষ: ${coupon.ends_at.slice(0, 16).replace("T", " ")}`}
            </p>
            <p>
              <button
                type="button"
                disabled={busyId === coupon.id}
                onClick={() => void onToggle(coupon)}
              >
                {coupon.active ? "বন্ধ করুন — Deactivate" : "চালু করুন — Activate"}
              </button>
              {api.remove && (
                <>
                  {" "}
                  <button
                    type="button"
                    disabled={busyId === coupon.id}
                    onClick={() => void onRemove(coupon)}
                  >
                    মুছে ফেলুন — Delete
                  </button>
                </>
              )}
            </p>
          </article>
        ))
      )}
    </>
  );
}
