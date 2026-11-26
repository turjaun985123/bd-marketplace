"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import {
  getBuyerRisk,
  getPayoutAccount,
  getSellerDashboard,
  listPayouts,
  payoutStatusLabel,
  requestPayout,
  riskLevelLabel,
  savePayoutAccount,
  stockStatusLabel,
  type BuyerRisk,
  type Payout,
  type PayoutAccount,
  type SellerDashboard,
} from "@/lib/api";
import { parseTakaToPoisha } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

const STATUS_LABELS: Record<string, string> = {
  placed: "অর্ডার পড়েছে",
  confirmed: "নিশ্চিত",
  shipped: "পাঠানো হয়েছে",
  delivered: "ডেলিভারি হয়েছে",
  cancelled: "বাতিল",
  returned: "ফেরত এসেছে",
};

/**
 * Seller home (Wave 9): the dashboard from GET /v1/seller/dashboard
 * (store, activity counts, earnings, recent sub-orders), the payout
 * account form, the payout request form, and the payout history.
 *
 * Money rules live on the API and are only displayed here: earnings
 * count DELIVERED sub-orders only (goods minus commission), the
 * request form converts the typed taka amount to poisha via the one
 * shared parser, and the API enforces the 500-taka minimum and the
 * available-balance fit. Payouts stay "অনুরোধ করা হয়েছে"
 * (requested) until platform approval lands with the admin wave.
 */
export default function SellerDashboardPage() {
  const { ready, token } = useAuth();
  const [dashboard, setDashboard] = useState<SellerDashboard | null>(null);
  const [account, setAccount] = useState<PayoutAccount | null>(null);
  const [payouts, setPayouts] = useState<Payout[]>([]);
  const [message, setMessage] = useState<string | null>(null);

  // Payout account form state.
  const [method, setMethod] = useState("bkash");
  const [accountName, setAccountName] = useState("");
  const [accountNumber, setAccountNumber] = useState("");
  const [accountBusy, setAccountBusy] = useState(false);

  // Payout request form state (amount typed in taka).
  const [amountTaka, setAmountTaka] = useState("");
  const [payoutBusy, setPayoutBusy] = useState(false);

  // Buyer-risk lookup state (Wave 17 fraud shield).
  const [riskPhone, setRiskPhone] = useState("");
  const [risk, setRisk] = useState<BuyerRisk | null>(null);
  const [riskBusy, setRiskBusy] = useState(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const [dash, acc, list] = await Promise.all([
        getSellerDashboard(token),
        getPayoutAccount(token),
        listPayouts(token, { limit: 20 }),
      ]);
      setDashboard(dash);
      setAccount(acc);
      setPayouts(list.items);
      if (acc) {
        setMethod(acc.method);
        setAccountName(acc.account_name);
        setAccountNumber(acc.account_number);
      }
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "ড্যাশবোর্ড লোড করা যায়নি।"
      );
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onSaveAccount(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setAccountBusy(true);
    setMessage(null);
    try {
      const saved = await savePayoutAccount(token, {
        method,
        account_name: accountName,
        account_number: accountNumber,
      });
      setAccount(saved);
      setMessage("পে-আউট অ্যাকাউন্ট সেভ হয়েছে। (Payout account saved.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "সেভ করা যায়নি।");
    } finally {
      setAccountBusy(false);
    }
  }

  async function onRequestPayout(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    const amountPoisha = parseTakaToPoisha(amountTaka);
    if (amountPoisha === null) {
      setMessage("সঠিক টাকার পরিমাণ লিখুন। (Enter a valid taka amount.)");
      return;
    }
    setPayoutBusy(true);
    setMessage(null);
    try {
      await requestPayout(token, amountPoisha);
      setAmountTaka("");
      setMessage("পে-আউট অনুরোধ পাঠানো হয়েছে। (Payout requested.)");
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "অনুরোধ করা যায়নি।");
    } finally {
      setPayoutBusy(false);
    }
  }

  async function onLookupRisk(e: React.FormEvent) {
    e.preventDefault();
    if (!token || !riskPhone.trim()) return;
    setRiskBusy(true);
    setMessage(null);
    try {
      setRisk(await getBuyerRisk(token, riskPhone.trim()));
    } catch (err) {
      setRisk(null);
      setMessage(err instanceof Error ? err.message : "ঝুঁকি দেখা যায়নি।");
    } finally {
      setRiskBusy(false);
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
        <h1>বিক্রেতা ড্যাশবোর্ড</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  const earnings = dashboard?.earnings;
  const counts = dashboard?.counts;

  return (
    <main>
      <h1>বিক্রেতা ড্যাশবোর্ড{dashboard ? ` — ${dashboard.store.name}` : ""}</h1>
      <p>
        <Link href="/seller/products">আমার পণ্য — My products</Link>
        {" · "}
        <Link href="/seller/inventory">ইনভেন্টরি — Inventory</Link>
        {" · "}
        <Link href="/seller/orders">আমার বিক্রির অর্ডার — Seller orders</Link>
        {" · "}
        <Link href="/seller/import">Facebook থেকে ইমপোর্ট — Import</Link>
        {" · "}
        <Link href="/seller/inbox">ইনবক্স — Inbox</Link>
        {" · "}
        <Link href="/seller/coupons">কুপন — Coupons</Link>
        {" · "}
        <Link href="/seller/returns">রিটার্ন — Returns</Link>
        {" · "}
        <Link href="/seller/branding">স্টোরের সাজসজ্জা — Branding</Link>
        {" · "}
        <Link href="/seller/verification">স্টোর যাচাইকরণ — Verification</Link>
      </p>
      {message && <p role="status">{message}</p>}
      {dashboard !== null && dashboard.low_stock_items.length > 0 && (
        <section>
          <div className="card">
            <h2>স্টক সতর্কতা — Low stock</h2>
            {dashboard.low_stock_items.map((item) => (
              <p key={item.product_id}>
                <span
                  className={`stock-badge stock-badge-${item.stock_status}`}
                >
                  {stockStatusLabel(item.stock_status)}
                </span>{" "}
                {item.title} — স্টক মাত্র {item.stock}টি
              </p>
            ))}
            <p>
              <Link href="/seller/inventory">
                ইনভেন্টরিতে গিয়ে স্টক ঠিক করুন — Fix stock in inventory
              </Link>
            </p>
          </div>
        </section>
      )}
      {dashboard === null ? (
        <p>লোড হচ্ছে…</p>
      ) : (
        <>
          <section>
            <h2>হিসাব — Money</h2>
            <div className="card">
              <p>
                মোট আয় (শুধু ডেলিভারি হওয়া অর্ডার থেকে):{" "}
                <strong>
                  <PriceTag amountPoisha={earnings?.earned_poisha ?? 0} />
                </strong>
              </p>
              <p>
                উপলব্ধ ব্যালেন্স:{" "}
                <strong>
                  <PriceTag amountPoisha={earnings?.available_poisha ?? 0} />
                </strong>
              </p>
              <p>
                পেন্ডিং পে-আউট:{" "}
                <PriceTag amountPoisha={earnings?.pending_poisha ?? 0} />
              </p>
              <p>
                মোট পে-আউট হয়েছে:{" "}
                <PriceTag amountPoisha={earnings?.paid_out_poisha ?? 0} />
              </p>
              <p>
                মোট পণ্য: {counts?.products ?? 0} · মোট সাব-অর্ডার:{" "}
                {counts
                  ? Object.values(counts.sub_orders_by_status).reduce(
                      (sum, n) => sum + n,
                      0
                    )
                  : 0}
              </p>
              {counts && (
                <p>
                  {Object.entries(counts.sub_orders_by_status).map(
                    ([status, count]) => (
                      <span key={status}>
                        {STATUS_LABELS[status] ?? status}: {count}{" "}
                      </span>
                    )
                  )}
                </p>
              )}
            </div>
          </section>

          <section>
            <h2>সাম্প্রতিক অর্ডার — Recent orders</h2>
            {dashboard.recent_sub_orders.length === 0 ? (
              <p>এখনো কোনো বিক্রির অর্ডার নেই। (No sales orders yet.)</p>
            ) : (
              dashboard.recent_sub_orders.map((sub) => (
                <article key={sub.id} className="card">
                  <p>
                    অবস্থা: {STATUS_LABELS[sub.status] ?? sub.status} (
                    {sub.status}) ·{" "}
                    {new Date(sub.created_at).toLocaleString("bn-BD")}
                  </p>
                  {sub.items.map((item) => (
                    <p key={item.id}>
                      {item.title_snapshot} × {item.quantity} —{" "}
                      <PriceTag amountPoisha={item.line_total_poisha} />
                    </p>
                  ))}
                  <p>
                    আপনার পাওনা:{" "}
                    <strong>
                      <PriceTag amountPoisha={sub.seller_payout_poisha} />
                    </strong>
                  </p>
                </article>
              ))
            )}
            <p>
              <Link href="/seller/orders">
                সব অর্ডার দেখুন — See all orders
              </Link>
            </p>
          </section>

          <section>
            <h2>পে-আউট অ্যাকাউন্ট — Payout account</h2>
            <form onSubmit={(e) => void onSaveAccount(e)}>
              <label>
                মাধ্যম (method)
                <select
                  value={method}
                  onChange={(e) => setMethod(e.target.value)}
                >
                  <option value="bkash">বিকাশ — bKash</option>
                  <option value="nagad">নগদ — Nagad</option>
                  <option value="bank">ব্যাংক — Bank</option>
                </select>
              </label>{" "}
              <label>
                অ্যাকাউন্টের নাম (account name)
                <input
                  value={accountName}
                  onChange={(e) => setAccountName(e.target.value)}
                  required
                />
              </label>{" "}
              <label>
                {method === "bank"
                  ? "অ্যাকাউন্ট নম্বর (account number)"
                  : "মোবাইল নম্বর (mobile number)"}
                <input
                  value={accountNumber}
                  onChange={(e) => setAccountNumber(e.target.value)}
                  required
                />
              </label>{" "}
              <button type="submit" disabled={accountBusy}>
                {accountBusy ? "সেভ হচ্ছে…" : "সেভ করুন"}
              </button>
            </form>
            {account && (
              <p>
                বর্তমান: {account.method} — {account.account_name} (
                {account.account_number})
              </p>
            )}
          </section>

          <section>
            <h2>পে-আউট অনুরোধ — Request payout</h2>
            <p>
              সর্বনিম্ন পে-আউট ৳৫০০। অনুরোধের পর টাকা পেন্ডিং থাকে;
              প্ল্যাটফর্ম অনুমোদন দিলে পরিশোধ হয়।
            </p>
            <form onSubmit={(e) => void onRequestPayout(e)}>
              <label>
                পরিমাণ (টাকায়) (amount, taka)
                <input
                  value={amountTaka}
                  onChange={(e) => setAmountTaka(e.target.value)}
                  inputMode="decimal"
                  placeholder="1000"
                  required
                />
              </label>{" "}
              <button type="submit" disabled={payoutBusy || !account}>
                {payoutBusy ? "পাঠানো হচ্ছে…" : "অনুরোধ করুন"}
              </button>
              {!account && (
                <span> আগে পে-আউট অ্যাকাউন্ট সেভ করুন।</span>
              )}
            </form>
            {payouts.length > 0 && (
              <>
                <h3>পে-আউট ইতিহাস — Payout history</h3>
                {payouts.map((payout) => (
                  <article key={payout.id} className="card">
                    <p>
                      <strong>
                        <PriceTag amountPoisha={payout.amount_poisha} />
                      </strong>{" "}
                      · {payoutStatusLabel(payout.status)} ({payout.status})
                      · {payout.method} — {payout.account_number} ·{" "}
                      {new Date(payout.created_at).toLocaleString("bn-BD")}
                    </p>
                  </article>
                ))}
              </>
            )}
          </section>

          <section>
            <h2>ক্রেতার ঝুঁকি যাচাই — Buyer risk check</h2>
            <p>
              COD জালিয়াতি শিল্ড: ক্রেতার মোবাইল নম্বর দিয়ে তার আগের
              অর্ডারের ইতিহাস দেখুন — কতটি ডেলিভারি হয়েছে, কতটি ফেরত
              এসেছে। উচ্চ-ঝুঁকির ক্রেতার অর্ডারে সার্ভার নিজে থেকেই
              অগ্রিম বাধ্যতামূলক করে।
            </p>
            <form onSubmit={(e) => void onLookupRisk(e)}>
              <label>
                ক্রেতার মোবাইল নম্বর (buyer phone)
                <input
                  value={riskPhone}
                  onChange={(e) => setRiskPhone(e.target.value)}
                  placeholder="01712345678"
                  inputMode="tel"
                  required
                />
              </label>{" "}
              <button type="submit" disabled={riskBusy}>
                {riskBusy ? "দেখা হচ্ছে…" : "ঝুঁকি দেখুন"}
              </button>
            </form>
            {risk &&
              (risk.found ? (
                <div className="card">
                  <p>
                    ঝুঁকির মাত্রা:{" "}
                    <strong>{riskLevelLabel(risk.risk_level)}</strong> (
                    {risk.risk_level})
                    {risk.requires_advance &&
                      " · এই ক্রেতার সব COD অর্ডারে অগ্রিম বাধ্যতামূলক"}
                  </p>
                  <p>
                    মোট অর্ডার: {risk.total_orders} · ডেলিভারি হয়েছে:{" "}
                    {risk.delivered} · ফেরত এসেছে: {risk.returned_rto} ·
                    ক্রেতা বাতিল করেছে: {risk.buyer_cancelled}
                  </p>
                </div>
              ) : (
                <p>
                  এই নম্বরে কোনো অ্যাকাউন্ট পাওয়া যায়নি — নতুন ক্রেতা
                  হিসেবে ঝুঁকি নিম্ন ধরা হয়েছে।
                </p>
              ))}
          </section>
        </>
      )}
    </main>
  );
}
