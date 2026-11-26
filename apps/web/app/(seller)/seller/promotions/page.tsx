"use client";

import { Suspense, useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import PriceTag from "@/components/PriceTag";
import {
  cancelPromotion,
  getPromotionPolicy,
  getSellerBalance,
  getSellerDashboard,
  getStoreProducts,
  listMyPromotions,
  promoteProduct,
  promotionStatusLabel,
  type Earnings,
  type Product,
  type PromotionPolicy,
  type SellerPromotion,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Seller promotions (Wave 26): buy sponsored placement for an
 * active product, prepaid from the earnings balance, and manage
 * the promotions already bought.
 *
 * The money rules live on the API and are only displayed here:
 * the price is the platform's effective per-day price, the cost
 * preview is days × that price, the API refuses when the
 * available balance cannot cover it or another promotion for
 * the product is live, and CANCELLING NEVER REFUNDS — that is
 * stated in the purchase copy below, before any button, not
 * discovered afterwards.
 */
export default function SellerPromotionsPage() {
  return (
    <Suspense fallback={<p>লোড হচ্ছে…</p>}>
      <Promotions />
    </Suspense>
  );
}

function Promotions() {
  const { ready, token } = useAuth();
  const searchParams = useSearchParams();
  const preselected = searchParams.get("product");
  const [policy, setPolicy] = useState<PromotionPolicy | null>(null);
  const [balance, setBalance] = useState<Earnings | null>(null);
  const [products, setProducts] = useState<Product[]>([]);
  const [promotions, setPromotions] = useState<SellerPromotion[]>([]);
  const [daysByProduct, setDaysByProduct] = useState<Record<string, string>>(
    {}
  );
  const [message, setMessage] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const [pol, bal, mine, dashboard] = await Promise.all([
        getPromotionPolicy(),
        getSellerBalance(token),
        listMyPromotions(token),
        getSellerDashboard(token),
      ]);
      setPolicy(pol);
      setBalance(bal);
      setPromotions(mine.items);
      const list = await getStoreProducts(dashboard.store.slug, token);
      setProducts(list.items.filter((p) => p.status === "active"));
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "প্রমোশন লোড করা যায়নি।"
      );
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onPromote(productId: string) {
    if (!token) return;
    const days = Number(daysByProduct[productId] ?? "7");
    if (!Number.isInteger(days) || days < 1 || days > (policy?.max_days ?? 30)) {
      setMessage(
        `দিন সংখ্যা ১ থেকে ${policy?.max_days ?? 30} এর মধ্যে হতে হবে।`
      );
      return;
    }
    setBusyId(productId);
    setMessage(null);
    try {
      const result = await promoteProduct(token, productId, days);
      setMessage(
        `প্রমোশন চালু হয়েছে — ${result.promotion.days} দিনের জন্য ` +
          `${result.cost_poisha / 100} টাকা ব্যালেন্স থেকে কাটা হয়েছে।`
      );
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "প্রমোট করা যায়নি।");
    } finally {
      setBusyId(null);
    }
  }

  async function onCancel(promotionId: string) {
    if (!token) return;
    setBusyId(promotionId);
    setMessage(null);
    try {
      await cancelPromotion(token, promotionId);
      setMessage(
        "প্রমোশন বাতিল হয়েছে — প্লেসমেন্ট বন্ধ। কাটা টাকা ফেরত হয়নি (নীতিমালা অনুযায়ী ফেরত হয় না)।"
      );
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "বাতিল করা যায়নি।");
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
        <h1>প্রমোশন — Promotions</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  const pricePerDay = policy?.price_per_day_poisha ?? 0;
  const liveProductIds = new Set(
    promotions
      .filter((p) => p.display_status === "active")
      .map((p) => p.product_id)
  );

  return (
    <main>
      <h1>প্রমোশন — Sponsored promotions</h1>
      <p>
        <Link href="/seller">ড্যাশবোর্ডে ফিরুন — Back to dashboard</Link>
      </p>
      {message && <p role="status">{message}</p>}

      <section>
        <div className="card">
          <h2>দাম ও ব্যালেন্স — Price &amp; balance</h2>
          <p>
            প্রতি দিনের দাম:{" "}
            <strong>
              <PriceTag amountPoisha={pricePerDay} />
            </strong>{" "}
            · সর্বোচ্চ {policy?.max_days ?? 30} দিন একবারে কেনা যায়।
          </p>
          <p>
            আপনার উপলব্ধ ব্যালেন্স:{" "}
            <strong>
              <PriceTag amountPoisha={balance?.available_poisha ?? 0} />
            </strong>{" "}
            (মোট আয়{" "}
            <PriceTag amountPoisha={balance?.earned_poisha ?? 0} /> · এখন
            পর্যন্ত প্রমোশনে কাটা হয়েছে{" "}
            <PriceTag amountPoisha={balance?.charges_total_poisha ?? 0} />)
          </p>
          <p>
            প্রমোশনের টাকা শুধুমাত্র আপনার আয়ের ব্যালেন্স থেকে কাটা
            হয় — আলাদা করে টাকা জমা দেওয়ার সুযোগ নেই, তাই বিক্রি
            না হলে প্রমোট করা যায় না।
          </p>
        </div>
      </section>

      <section>
        <h2>পণ্য প্রমোট করুন — Promote a product</h2>
        <p>
          <strong>
            মনোযোগ দিন: প্রমোশনের টাকা আপনার ব্যালেন্স থেকে কাটা হবে,
            এবং মাঝপথে বাতিল করলেও সেই টাকা ফেরত পাওয়া যাবে না।
          </strong>{" "}
          বাতিল করলে প্লেসমেন্ট সঙ্গে সঙ্গে বন্ধ হয়ে যায়, কিন্তু
          কাটা চার্জ ফেরত হয় না।
        </p>
        {products.length === 0 ? (
          <p>
            আপনার কোনো সক্রিয় (active) পণ্য নেই — আগে{" "}
            <Link href="/seller/products">পণ্যের পেজে</Link> গিয়ে পণ্য
            সক্রিয় করুন।
          </p>
        ) : (
          products.map((p) => {
            const daysText = daysByProduct[p.id] ?? "7";
            const days = Number(daysText);
            const cost =
              Number.isInteger(days) && days > 0 ? days * pricePerDay : 0;
            const live = liveProductIds.has(p.id) || p.is_promoted;
            return (
              <article
                key={p.id}
                className="card"
                style={
                  preselected === p.id
                    ? { outline: "2px solid #6a1b9a" }
                    : undefined
                }
              >
                <h3>
                  {p.title_bn ?? p.title}{" "}
                  {live && (
                    <span className="sponsored-badge">
                      স্পন্সরড — এখন লাইভ
                    </span>
                  )}
                </h3>
                <p>
                  দাম: <PriceTag amountPoisha={p.price_poisha} /> · স্টক:{" "}
                  {p.stock}
                </p>
                {live ? (
                  <p>
                    এই পণ্যের একটি প্রমোশন ইতিমধ্যে চলছে — নিচের তালিকা
                    থেকে দেখুন। একটি পণ্যে একসঙ্গে একটির বেশি লাইভ
                    প্রমোশন চলে না।
                  </p>
                ) : (
                  <form
                    onSubmit={(e) => {
                      e.preventDefault();
                      void onPromote(p.id);
                    }}
                  >
                    <label>
                      কত দিন? (দিন)
                      <input
                        value={daysText}
                        onChange={(e) =>
                          setDaysByProduct((prev) => ({
                            ...prev,
                            [p.id]: e.target.value,
                          }))
                        }
                        inputMode="numeric"
                        required
                      />
                    </label>{" "}
                    <span>
                      খরচ: <PriceTag amountPoisha={cost} /> — ব্যালেন্স
                      থেকে কাটা হবে, বাতিলে ফেরত নেই।
                    </span>{" "}
                    <button type="submit" disabled={busyId === p.id}>
                      {busyId === p.id ? "চালু হচ্ছে…" : "প্রমোট করুন"}
                    </button>
                  </form>
                )}
              </article>
            );
          })
        )}
      </section>

      <section>
        <h2>আমার প্রমোশন — My promotions</h2>
        {promotions.length === 0 ? (
          <p>এখনো কোনো প্রমোশন কেনা হয়নি। (No promotions yet.)</p>
        ) : (
          promotions.map((promotion) => (
            <article key={promotion.id} className="card">
              <p>
                <strong>{promotion.product_title}</strong> ·{" "}
                {promotionStatusLabel(promotion.display_status)} (
                {promotion.display_status})
              </p>
              <p>
                {promotion.days} দিন · খরচ:{" "}
                <PriceTag amountPoisha={promotion.cost_poisha} /> ·{" "}
                {new Date(promotion.starts_at).toLocaleString("bn-BD")}{" "}
                থেকে {new Date(promotion.ends_at).toLocaleString("bn-BD")}{" "}
                পর্যন্ত
              </p>
              {promotion.display_status === "active" && (
                <p>
                  <button
                    type="button"
                    disabled={busyId === promotion.id}
                    onClick={() => void onCancel(promotion.id)}
                  >
                    {busyId === promotion.id
                      ? "বাতিল হচ্ছে…"
                      : "বাতিল করুন — টাকা ফেরত হবে না"}
                  </button>
                </p>
              )}
            </article>
          ))
        )}
      </section>
    </main>
  );
}
