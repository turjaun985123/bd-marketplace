"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import SellerFlashCard from "@/components/SellerFlashCard";
import SellerFollowersCard from "@/components/SellerFollowersCard";
import StoreSwitcher from "@/components/StoreSwitcher";
import {
  downloadSellerExport,
  getBuyerRisk,
  getMySubscription,
  getPayoutAccount,
  getSellerDashboard,
  listPayouts,
  listSellerStores,
  payoutStatusLabel,
  planLabel,
  requestPayout,
  riskLevelLabel,
  savePayoutAccount,
  stockStatusLabel,
  storeRoleLabel,
  type BuyerRisk,
  type Payout,
  type PayoutAccount,
  type SellerDashboard,
  type SellerStore,
  type SubscriptionView,
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
 *
 * Wave 31: the footer section downloads the seller's own record
 * (GET /v1/seller/export) as one JSON file — the portability
 * export, fetched as a Blob with the auth header and saved via
 * an object URL, the Wave 27 CSV pattern.
 *
 * Wave 36: a plan badge under the store switcher shows the
 * store's current subscription plan (GET /v1/seller/subscription)
 * and links to the subscription page; the badge is best-effort
 * and never blocks the dashboard.
 *
 * Wave 40: a fulfillment summary card (confirm / booking /
 * attention counts + the SLA breach tally from the dashboard's
 * `fulfillment` block) links into the fulfillment center; it
 * renders only when there is work waiting.
 */
export default function SellerDashboardPage() {
  const { ready, token } = useAuth();
  const [dashboard, setDashboard] = useState<SellerDashboard | null>(null);
  const [account, setAccount] = useState<PayoutAccount | null>(null);
  const [payouts, setPayouts] = useState<Payout[]>([]);
  const [message, setMessage] = useState<string | null>(null);

  // Wave 34 team stores + the switcher selection. "" is the
  // default whole-activity view; a store id scopes the
  // dashboard to that one store (the payout sections below
  // belong to the caller's OWN store and hide in scoped view).
  const [stores, setStores] = useState<SellerStore[]>([]);
  const [selectedStoreId, setSelectedStoreId] = useState("");
  const [selectionReady, setSelectionReady] = useState(false);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    (async () => {
      try {
        const list = await listSellerStores(token);
        if (cancelled) return;
        setStores(list);
        const saved = window.localStorage.getItem("seller-store-id");
        if (saved && list.some((store) => store.id === saved)) {
          setSelectedStoreId(saved);
        }
      } catch {
        // The stores list only powers the switcher; the
        // dashboard itself still loads without it.
      } finally {
        if (!cancelled) setSelectionReady(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [token]);

  function onSelectStore(storeId: string) {
    setSelectedStoreId(storeId);
    if (storeId) {
      window.localStorage.setItem("seller-store-id", storeId);
    } else {
      window.localStorage.removeItem("seller-store-id");
    }
  }

  // Payout account form state.
  const [method, setMethod] = useState("bkash");
  const [accountName, setAccountName] = useState("");
  const [accountNumber, setAccountNumber] = useState("");
  const [accountBusy, setAccountBusy] = useState(false);

  // Payout request form state (amount typed in taka).
  const [amountTaka, setAmountTaka] = useState("");
  const [payoutBusy, setPayoutBusy] = useState(false);
  // Wave 44: ONE idempotency key per payout-form session,
  // minted lazily at the session's first submit (see
  // onRequestPayout) and sent with every attempt, so a double
  // click, a browser retry or a re-submit after a network
  // error all carry the SAME key and the API hands back the
  // original payout instead of creating a duplicate request.
  // Only a SUCCESS rotates the key: the next, genuinely new
  // request must not be swallowed as a replay of this one.
  const payoutKeyRef = useRef<string | null>(null);

  // Buyer-risk lookup state (Wave 17 fraud shield).
  const [riskPhone, setRiskPhone] = useState("");
  const [risk, setRisk] = useState<BuyerRisk | null>(null);
  const [riskBusy, setRiskBusy] = useState(false);

  // Data export state (Wave 31 portability).
  const [exportBusy, setExportBusy] = useState(false);

  // Subscription plan badge (Wave 36): the plan of the store
  // on screen, fetched beside the dashboard; a failure here
  // never blocks the dashboard itself.
  const [subscription, setSubscription] = useState<SubscriptionView | null>(
    null
  );

  const load = useCallback(async () => {
    if (!token || !selectionReady) return;
    // Wave 36: the plan badge for the store on screen (the
    // selected store, or the payout store in the default
    // view). Best-effort: the badge hides on any failure.
    void getMySubscription(token, selectedStoreId || undefined)
      .then(setSubscription)
      .catch(() => setSubscription(null));
    try {
      if (selectedStoreId) {
        // Scoped view: the dashboard for one served store.
        // Payout account/history are the caller's own-store
        // surfaces and are not fetched here (they hide below).
        setDashboard(await getSellerDashboard(token, selectedStoreId));
        return;
      }
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
  }, [token, selectedStoreId, selectionReady]);

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
      const idempotencyKey = payoutKeyRef.current ?? crypto.randomUUID();
      payoutKeyRef.current = idempotencyKey;
      await requestPayout(token, amountPoisha, idempotencyKey);
      // Success: rotate to a fresh key (see the ref's comment)
      // so the next request is a new request, not a replay.
      payoutKeyRef.current = crypto.randomUUID();
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

  // Wave 31: download the seller's own record (store, full
  // catalogue, sub-orders, payouts, coupons, promotions) as
  // one JSON file — the portability export.
  async function onDownloadExport() {
    if (!token) return;
    setExportBusy(true);
    setMessage(null);
    try {
      const blob = await downloadSellerExport(token);
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = "bd-marketplace-seller-export.json";
      anchor.click();
      URL.revokeObjectURL(url);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "এক্সপোর্ট ডাউনলোড করা যায়নি।"
      );
    } finally {
      setExportBusy(false);
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
  // Wave 34: the caller's role in the store on screen, and
  // whether this is a scoped (single served store) view.
  const myRole = dashboard
    ? stores.find((store) => store.id === dashboard.store.id)?.my_role
    : undefined;
  const scoped = selectedStoreId !== "";

  return (
    <main>
      <h1>
        বিক্রেতা ড্যাশবোর্ড{dashboard ? ` — ${dashboard.store.name}` : ""}
        {myRole ? ` (${storeRoleLabel(myRole)})` : ""}
      </h1>
      <StoreSwitcher
        stores={stores}
        value={selectedStoreId}
        onChange={onSelectStore}
      />
      {subscription && (
        <p>
          বর্তমান প্ল্যান: <strong>{planLabel(subscription.plan)}</strong>
          {subscription.plan !== "free" && subscription.expires_at
            ? ` · মেয়াদ শেষ ${new Date(
                subscription.expires_at
              ).toLocaleDateString("bn-BD")}`
            : ""}{" "}
          · <Link href="/seller/subscription">প্ল্যান দেখুন — View plan</Link>
        </p>
      )}
      <p>
        <Link href="/seller/products">আমার পণ্য — My products</Link>
        {" · "}
        <Link href="/seller/inventory">ইনভেন্টরি — Inventory</Link>
        {" · "}
        <Link href="/seller/orders">আমার বিক্রির অর্ডার — Seller orders</Link>
        {" · "}
        <Link href="/seller/fulfillment">ফুলফিলমেন্ট — Fulfillment</Link>
        {" · "}
        <Link href="/seller/import">Facebook থেকে ইমপোর্ট — Import</Link>
        {" · "}
        <Link href="/seller/bulk">বাল্ক ক্যাটালগ — Bulk catalogue</Link>
        {" · "}
        <Link href="/seller/inbox">ইনবক্স — Inbox</Link>
        {" · "}
        <Link href="/seller/questions">প্রশ্নোত্তর — Q&amp;A</Link>
        {" · "}
        <Link href="/seller/coupons">কুপন — Coupons</Link>
        {" · "}
        <Link href="/seller/promotions">প্রমোশন — Promotions</Link>
        {" · "}
        <Link href="/seller/flash-sale">ফ্ল্যাশ সেল — Flash sale</Link>
        {" · "}
        <Link href="/seller/subscription">সাবস্ক্রিপশন — Subscription</Link>
        {" · "}
        <Link href="/seller/api-keys">API কী — API keys</Link>
        {" · "}
        <Link href="/seller/media">মিডিয়া লাইব্রেরি — Media library</Link>
        {" · "}
        <Link href="/seller/analytics">অ্যানালিটিক্স — Analytics</Link>
        {" · "}
        <Link href="/seller/returns">রিটার্ন — Returns</Link>
        {" · "}
        <Link href="/seller/branding">স্টোরের সাজসজ্জা — Branding</Link>
        {" · "}
        <Link href="/seller/storefront">স্টোরফ্রন্ট এডিটর — Storefront editor</Link>
        {" · "}
        <Link href="/seller/followers">ফলোয়ার — Followers</Link>
        {" · "}
        <Link href="/seller/collections">কালেকশন — Collections</Link>
        {" · "}
        <Link href="/seller/verification">স্টোর যাচাইকরণ — Verification</Link>
        {myRole !== "staff" && (
          <>
            {" · "}
            <Link href="/seller/members">টিম — Team</Link>
          </>
        )}
      </p>
      {message && <p role="status">{message}</p>}
      {/* Wave 52: the live flash sale, when it carries this
          seller's products (renders nothing otherwise). */}
      {token && <SellerFlashCard token={token} />}
      {/* Wave 54 extension: the follower card (renders
          nothing until the first follow). */}
      {token && <SellerFollowersCard token={token} />}
      {dashboard !== null && dashboard.unanswered_questions > 0 && (
        <section>
          <div className="card">
            <h2>ক্রেতাদের প্রশ্ন — Buyer questions</h2>
            <p>
              {dashboard.unanswered_questions}টি প্রশ্ন উত্তরের অপেক্ষায়
              আছে।
            </p>
            <p>
              <Link href="/seller/questions">
                প্রশ্নোত্তর ইনবক্সে গিয়ে উত্তর দিন — Answer in the Q&amp;A
                inbox
              </Link>
            </p>
          </div>
        </section>
      )}
      {dashboard !== null &&
        (dashboard.fulfillment.to_confirm > 0 ||
          dashboard.fulfillment.to_ship > 0 ||
          dashboard.fulfillment.attention > 0) && (
          <section>
            <div className="card">
              <h2>ফুলফিলমেন্ট — Fulfillment</h2>
              <p>
                কনফার্ম বাকি: {dashboard.fulfillment.to_confirm} · বুকিং
                বাকি: {dashboard.fulfillment.to_ship} · মনোযোগ দিন:{" "}
                {dashboard.fulfillment.attention}
                {dashboard.fulfillment.breached > 0 && (
                  <>
                    {" "}
                    ·{" "}
                    <span className="sla-badge">
                      SLA ছাড়িয়েছে {dashboard.fulfillment.breached}টি
                    </span>
                  </>
                )}
              </p>
              <p>
                <Link href="/seller/fulfillment">
                  ফুলফিলমেন্ট সেন্টারে গিয়ে কাজ করুন — Work the queue
                </Link>
              </p>
            </div>
          </section>
        )}
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
                প্রমোশনে কাটা হয়েছে:{" "}
                <PriceTag
                  amountPoisha={earnings?.charges_total_poisha ?? 0}
                />{" "}
                <Link href="/seller/promotions">প্রমোশন দেখুন</Link>
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

          {scoped && (
            <p>
              এই স্টোরের পে-আউট শুধু স্টোরের মালিক পরিচালনা করেন —
              payouts for this store are managed by its owner.
            </p>
          )}
          {!scoped && (
            <>
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

          <section>
            <h2>ডেটা এক্সপোর্ট — Data export</h2>
            <p>
              আপনার স্টোরের সব তথ্য একটি JSON ফাইলে ডাউনলোড করুন:
              স্টোর, পুরো পণ্য তালিকা (খসড়াসহ), সাব-অর্ডার ও আইটেম,
              পে-আউট, কুপন আর প্রমোশন। এটি আপনার নিজের রেকর্ড —
              প্ল্যাটফর্ম ছাড়লেও হিসাব আপনার কাছে থাকবে।
            </p>
            <button
              type="button"
              onClick={() => void onDownloadExport()}
              disabled={exportBusy}
            >
              {exportBusy ? "তৈরি হচ্ছে…" : "ডেটা এক্সপোর্ট ডাউনলোড"}
            </button>
          </section>
            </>
          )}
        </>
      )}
    </main>
  );
}
