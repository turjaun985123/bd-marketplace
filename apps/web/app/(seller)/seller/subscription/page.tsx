"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import {
  cancelSubscription,
  getMySubscription,
  getSellerBalance,
  getSubscriptionPlans,
  isUnlimitedCap,
  listSellerStores,
  planLabel,
  subscribePlan,
  type Earnings,
  type SellerStore,
  type SubscriptionPlan,
  type SubscriptionView,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Seller subscription (Wave 36): the store's plan — free,
 * pro or business — bought with the earnings balance for a
 * 30-day period.
 *
 * The money rules live on the API and are only displayed
 * here: subscribing charges the plan's effective price from
 * the available earnings balance (the same balance payouts
 * draw on), a live paid plan discounts the store's checkout
 * commission and raises its product cap, switching plans
 * charges the new plan's full price with no proration, and
 * switching to free cancels the paid plan at once with no
 * refund. Cancelling stops only the RENEWAL — benefits run
 * to the paid period's end. Only the store's owner may
 * subscribe or cancel (the API enforces it); managers see
 * the plan read-only, staff do not get this page's actions.
 */
export default function SellerSubscriptionPage() {
  const { ready, token } = useAuth();
  const [plans, setPlans] = useState<SubscriptionPlan[]>([]);
  const [current, setCurrent] = useState<SubscriptionView | null>(null);
  const [balance, setBalance] = useState<Earnings | null>(null);
  const [myRole, setMyRole] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busyPlan, setBusyPlan] = useState<string | null>(null);
  const [cancelBusy, setCancelBusy] = useState(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const [catalogue, view, earnings, stores] = await Promise.all([
        getSubscriptionPlans(token),
        getMySubscription(token),
        getSellerBalance(token),
        listSellerStores(token),
      ]);
      setPlans(catalogue.items);
      setCurrent(view);
      setBalance(earnings);
      const mine = stores.find((store: SellerStore) => store.id === view.store_id);
      setMyRole(mine ? mine.my_role : null);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "সাবস্ক্রিপশন লোড করা যায়নি।"
      );
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  const isOwner = myRole === "owner";

  async function onSubscribe(plan: string) {
    if (!token) return;
    setBusyPlan(plan);
    setMessage(null);
    try {
      const result = await subscribePlan(token, plan);
      setCurrent(result.subscription);
      setMessage(
        result.charged_poisha > 0
          ? `${planLabel(plan)} চালু হয়েছে — ব্যালেন্স থেকে কাটা হয়েছে।`
          : `${planLabel(plan)} প্ল্যানে ফিরে গেছেন।`
      );
      void load();
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "সাবস্ক্রাইব করা যায়নি।"
      );
    } finally {
      setBusyPlan(null);
    }
  }

  async function onCancelRenewal() {
    if (!token) return;
    setCancelBusy(true);
    setMessage(null);
    try {
      const view = await cancelSubscription(token);
      setCurrent(view);
      setMessage(
        "অটো-রিনিউ বন্ধ হয়েছে — বর্তমান মেয়াদ শেষ হলে প্ল্যান ফ্রি হয়ে যাবে।"
      );
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "রিনিউ বাতিল করা যায়নি।"
      );
    } finally {
      setCancelBusy(false);
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
        <h1>সাবস্ক্রিপশন</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>সাবস্ক্রিপশন — Subscription</h1>
      <p>
        <Link href="/seller">ড্যাশবোর্ডে ফিরুন — Back to dashboard</Link>
      </p>
      {message && <p role="status">{message}</p>}

      <section>
        <h2>বর্তমান প্ল্যান — Current plan</h2>
        {current ? (
          <div className="card">
            <p>
              <strong>{planLabel(current.plan)}</strong>
              {current.status === "active" && current.plan !== "free"
                ? " · সক্রিয়"
                : ""}
              {current.status === "expired" ? " · মেয়াদ শেষ" : ""}
              {current.status === "cancelled" ? " · বাতিল করা" : ""}
            </p>
            {current.expires_at && current.plan !== "free" && (
              <p>
                মেয়াদ শেষ:{" "}
                {new Date(current.expires_at).toLocaleString("bn-BD")}
                {current.auto_renew
                  ? " · অটো-রিনিউ চালু আছে"
                  : " · অটো-রিনিউ বন্ধ"}
              </p>
            )}
            <p>
              কমিশন ছাড়: {current.commission_discount_bps / 100}% · পণ্যের
              সীমা:{" "}
              {isUnlimitedCap(current.product_cap)
                ? "সীমাহীন"
                : current.product_cap}
            </p>
            {balance && (
              <p>
                উপলব্ধ ব্যালেন্স:{" "}
                <PriceTag amountPoisha={balance.available_poisha} />
              </p>
            )}
            {isOwner &&
              current.plan !== "free" &&
              current.status === "active" &&
              current.auto_renew && (
                <button
                  type="button"
                  onClick={() => void onCancelRenewal()}
                  disabled={cancelBusy}
                >
                  {cancelBusy ? "বন্ধ করা হচ্ছে…" : "অটো-রিনিউ বন্ধ করুন"}
                </button>
              )}
          </div>
        ) : (
          <p>লোড হচ্ছে…</p>
        )}
      </section>

      <section>
        <h2>প্ল্যান বেছে নিন — Choose a plan</h2>
        <p>
          ব্যালেন্স থেকে কাটা হবে — আলাদা পেমেন্ট লাগবে না। প্ল্যান বদলালে
          নতুন প্ল্যানের পুরো দাম কাটা হবে এবং নতুন ৩০ দিনের মেয়াদ এখন
          থেকে শুরু হবে — আগের মেয়াদের বাকি দিনের জন্য কোনো ছাড় বা ফেরত
          নেই। ফ্রি প্ল্যানে ফিরলে পেইড প্ল্যান সঙ্গে সঙ্গে বাতিল হয়,
          ফেরতও নেই।
        </p>
        {!isOwner && (
          <p>
            শুধু স্টোরের মালিক সাবস্ক্রিপশন কিনতে বা বদলাতে পারেন — আপনি
            এই স্টোরে {myRole ?? ""} হিসেবে আছেন, তাই বোতামগুলো বন্ধ।
          </p>
        )}
        {plans.map((plan) => {
          const isCurrent = current?.plan === plan.key;
          return (
            <article key={plan.key} className="card">
              <h3>{planLabel(plan.key)}</h3>
              <p>
                {plan.price_poisha > 0 ? (
                  <>
                    <PriceTag amountPoisha={plan.price_poisha} /> /{" "}
                    {plan.period_days} দিন
                  </>
                ) : (
                  "বিনামূল্যে"
                )}{" "}
                · কমিশন ছাড়: {plan.commission_discount_bps / 100}% · পণ্যের
                সীমা:{" "}
                {isUnlimitedCap(plan.product_cap)
                  ? "সীমাহীন"
                  : plan.product_cap}
              </p>
              <button
                type="button"
                disabled={!isOwner || isCurrent || busyPlan !== null}
                onClick={() => void onSubscribe(plan.key)}
              >
                {isCurrent
                  ? "বর্তমান প্ল্যান"
                  : busyPlan === plan.key
                    ? "কেনা হচ্ছে…"
                    : plan.key === "free"
                      ? "ফ্রি প্ল্যানে ফিরুন"
                      : "সাবস্ক্রাইব করুন"}
              </button>
            </article>
          );
        })}
      </section>
    </main>
  );
}
