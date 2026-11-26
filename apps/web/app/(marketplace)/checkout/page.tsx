"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import {
  devSimulateSuccess,
  getAdvancePolicy,
  getLoyalty,
  getOrder,
  getPaymentMethods,
  paymentStatusLabel,
  placeOrder,
  previewCoupon,
  type AdvancePolicy,
  type CartItem,
  type CouponPreview,
  type LoyaltyView,
  type Order,
  type PaymentMethodInfo,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";
import { useCart } from "@/hooks/useCart";
import {
  useDeliveryCharge,
  type DeliveryZone,
} from "@/hooks/useDeliveryCharge";

/** Bangla labels for payment methods (availability comes from the API). */
const METHOD_LABELS: Record<string, string> = {
  cod: "ক্যাশ অন ডেলিভারি (COD) — পণ্য হাতে পেয়ে টাকা দিন",
  bkash: "বিকাশ (bKash)",
  nagad: "নগদ (Nagad)",
  sslcommerz: "SSLCommerz — কার্ড / ব্যাংক / মোবাইল ব্যাংকিং",
  shurjopay: "ShurjoPay",
};

/** Zone values the API expects, with their Bangla labels. */
const ZONES: { value: string; apiZone: DeliveryZone; label: string }[] = [
  { value: "inside_dhaka", apiZone: "InsideDhaka", label: "ঢাকার ভিতরে" },
  { value: "dhaka_suburb", apiZone: "DhakaSuburb", label: "ঢাকার উপশহর" },
  { value: "outside_dhaka", apiZone: "OutsideDhaka", label: "ঢাকার বাইরে" },
];

interface SellerGroup {
  storeSlug: string;
  items: CartItem[];
  goodsPoisha: number;
  /**
   * Billable weight in whole kg: the group's total grams (unit weight
   * x quantity, Wave 4) rounded UP per started kilogram — the same
   * rule the server applies in build_order_totals.
   */
  weightKg: number;
}

function groupByStore(items: CartItem[]): SellerGroup[] {
  const groups = new Map<string, SellerGroup & { totalGrams: number }>();
  for (const item of items) {
    let group = groups.get(item.store_slug);
    if (!group) {
      group = {
        storeSlug: item.store_slug,
        items: [],
        goodsPoisha: 0,
        weightKg: 0,
        totalGrams: 0,
      };
      groups.set(item.store_slug, group);
    }
    group.items.push(item);
    group.goodsPoisha += item.line_total_poisha;
    group.totalGrams += item.weight_grams * item.quantity;
  }
  return Array.from(groups.values()).map(
    ({ totalGrams, ...group }): SellerGroup => ({
      ...group,
      weightKg: Math.max(1, Math.ceil(totalGrams / 1000)),
    })
  );
}

/**
 * Checkout (COD + online methods from GET /v1/payments/methods,
 * Wave 5). The per-seller delivery preview is display-only,
 * quoted through the shared useDeliveryCharge hook (one quote per
 * seller group, billed on the group's real weight in grams — the
 * server's Wave 4 rule). The server's totals in the place-order
 * response are the final truth and are what the success screen shows.
 *
 * Wave 14: an optional coupon code is checked through the preview
 * endpoint (a store coupon needs a second preview carrying that
 * store's goods share, which only the cart knows) and shown as a
 * discount line; the code rides along with the order and checkout
 * re-validates it — the checkout response remains the authority.
 *
 * Wave 17: a COD order at/above the platform threshold (from
 * GET /v1/payments/advance-policy) may need a partial advance,
 * so the page warns early and offers an advance-method picker;
 * a buyer whose history forces an advance cannot be previewed,
 * so when checkout answers its advance 400 the picker appears
 * and the buyer simply places the order again with a method.
 * The success screen then walks the buyer through paying the
 * advance (the Wave 5 simulate block, reused) before anything
 * ships.
 *
 * Wave 35: a buyer with loyalty points sees a points block —
 * their balance, the most this cart may redeem (balance capped
 * at half the post-coupon goods, the server's rule previewed
 * with the same arithmetic), and an opt-in amount. The request
 * rides along with the order; the server clamps it and the
 * placed order reports the points actually used.
 */
export default function CheckoutPage() {
  const { ready, token } = useAuth();
  const { items, goodsTotalPoisha, loading, refresh } = useCart();
  const { quoteAmount } = useDeliveryCharge();

  const [name, setName] = useState("");
  const [phone, setPhone] = useState("");
  const [address, setAddress] = useState("");
  const [zone, setZone] = useState(ZONES[0]);
  const [charges, setCharges] = useState<Record<string, number | null>>({});
  const [placing, setPlacing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [placed, setPlaced] = useState<Order | null>(null);
  // Payment methods come from GET /v1/payments/methods — the API is
  // the single source of truth for what is available; this page only
  // renders that answer and tracks the buyer's selection.
  const [methods, setMethods] = useState<PaymentMethodInfo[] | null>(null);
  const [method, setMethod] = useState("cod");
  const [simulating, setSimulating] = useState(false);
  // Coupon (Wave 14): the typed code, and the last preview answer.
  const [couponCode, setCouponCode] = useState("");
  const [coupon, setCoupon] = useState<CouponPreview | null>(null);
  const [applyingCoupon, setApplyingCoupon] = useState(false);
  // Advance (Wave 17): the public policy, the buyer's chosen
  // advance method, and whether checkout has told us an advance
  // method is required (its 400) so the picker must show even
  // when the preview total is below the threshold.
  const [policy, setPolicy] = useState<AdvancePolicy | null>(null);
  const [advanceMethod, setAdvanceMethod] = useState("bkash");
  const [advanceRequired, setAdvanceRequired] = useState(false);
  // Loyalty (Wave 35): the buyer's points view, whether they
  // want to spend points on this order, and how many. The
  // server clamps the request to the redeemable maximum; the
  // figures previewed here use the same rule (balance, capped
  // at half the post-coupon goods) so the preview should match
  // — the placed order remains the authority.
  const [loyalty, setLoyalty] = useState<LoyaltyView | null>(null);
  const [useLoyalty, setUseLoyalty] = useState(false);
  const [loyaltyPointsInput, setLoyaltyPointsInput] = useState("");

  const groups = useMemo(() => groupByStore(items), [items]);

  // Quote one delivery charge per seller group whenever the cart or the
  // zone changes (preview only — the server recomputes at order time).
  useEffect(() => {
    let cancelled = false;
    if (groups.length === 0) {
      setCharges({});
      return;
    }
    void (async () => {
      const next: Record<string, number | null> = {};
      for (const group of groups) {
        next[group.storeSlug] = await quoteAmount(
          zone.apiZone,
          group.weightKg,
          group.goodsPoisha
        );
      }
      if (!cancelled) setCharges(next);
    })();
    return () => {
      cancelled = true;
    };
  }, [groups, zone, quoteAmount]);

  // Load the payment method availability once.
  useEffect(() => {
    let cancelled = false;
    getPaymentMethods()
      .then((list) => {
        if (!cancelled) setMethods(list);
      })
      .catch(() => {
        // API offline: COD stays selectable; the server decides at
        // order time anyway and its error is shown to the buyer.
        if (!cancelled) setMethods([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Load the COD advance policy once (public endpoint).
  useEffect(() => {
    let cancelled = false;
    getAdvancePolicy()
      .then((loaded) => {
        if (!cancelled) setPolicy(loaded);
      })
      .catch(() => {
        // No policy: no early warning; the server still decides
        // at order time and its answer drives the picker.
        if (!cancelled) setPolicy(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Load the buyer's loyalty balance once (Wave 35). A buyer
  // with no points simply never sees the loyalty block.
  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getLoyalty(token)
      .then((loaded) => {
        if (!cancelled) setLoyalty(loaded);
      })
      .catch(() => {
        if (!cancelled) setLoyalty(null);
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  const previewDelivery = groups.reduce(
    (sum, g) => sum + (charges[g.storeSlug] ?? 0),
    0
  );
  const previewKnown = groups.every((g) => charges[g.storeSlug] != null);

  // Loyalty preview (Wave 35): the most points this cart could
  // redeem — the balance, capped at half the post-coupon goods
  // (the server's rule, previewed with the same arithmetic) —
  // and the buyer's requested amount clamped to it. An empty
  // input means "use as many as possible".
  const postCouponGoodsPreview =
    goodsTotalPoisha - (coupon?.valid ? coupon.discount_poisha : 0);
  const loyaltyMaxUsable = loyalty
    ? Math.min(
        loyalty.available_points,
        Math.floor(
          Math.floor(postCouponGoodsPreview / 2) / loyalty.point_value_poisha
        )
      )
    : 0;
  const loyaltyRequestedParsed = Number.parseInt(loyaltyPointsInput, 10);
  const loyaltyPointsPreview =
    useLoyalty && loyalty
      ? Math.min(
          Number.isNaN(loyaltyRequestedParsed)
            ? loyaltyMaxUsable
            : Math.max(0, loyaltyRequestedParsed),
          loyaltyMaxUsable
        )
      : 0;
  const loyaltyDiscountPreview = loyalty
    ? loyaltyPointsPreview * loyalty.point_value_poisha
    : 0;

  // The grand total the advance threshold is measured against:
  // goods minus a valid coupon's discount and the loyalty
  // discount, plus delivery.
  const previewGrand =
    goodsTotalPoisha -
    (coupon?.valid ? coupon.discount_poisha : 0) -
    loyaltyDiscountPreview +
    previewDelivery;
  // Show the advance picker when the preview says an advance is
  // likely (COD + threshold reached) or checkout already asked
  // for a method (a risk-forced advance below the threshold).
  const showAdvancePicker =
    method === "cod" &&
    (advanceRequired ||
      (policy !== null &&
        policy.threshold_poisha > 0 &&
        previewKnown &&
        previewGrand >= policy.threshold_poisha));

  // A coupon preview is priced against the cart it was applied to;
  // when the cart's goods total changes, the preview is stale.
  useEffect(() => {
    setCoupon(null);
  }, [goodsTotalPoisha]);

  async function onApplyCoupon() {
    if (!token || !couponCode.trim()) return;
    setApplyingCoupon(true);
    setError(null);
    try {
      let preview = await previewCoupon(token, {
        code: couponCode.trim(),
        goods_total_poisha: goodsTotalPoisha,
      });
      // A store coupon's discount depends on that store's goods in
      // the cart — the first preview only reveals the scope, so
      // re-preview with the store's share once it is known.
      if (preview.scope === "store" && preview.store_id) {
        const storeGoods = items
          .filter((item) => item.store_id === preview.store_id)
          .reduce((sum, item) => sum + item.line_total_poisha, 0);
        preview = await previewCoupon(token, {
          code: couponCode.trim(),
          goods_total_poisha: goodsTotalPoisha,
          store_id: preview.store_id,
          store_goods_poisha: storeGoods,
        });
      }
      setCoupon(preview);
    } catch (err) {
      setCoupon(null);
      setError(err instanceof Error ? err.message : "কুপন যাচাই করা যায়নি।");
    } finally {
      setApplyingCoupon(false);
    }
  }

  async function onPlaceOrder(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setPlacing(true);
    setError(null);
    try {
      const order = await placeOrder(token, {
        delivery_name: name.trim(),
        delivery_phone: phone.trim(),
        delivery_address: address.trim(),
        delivery_zone: zone.value,
        payment_method: method,
        // Only a preview-valid code is sent; checkout re-checks it
        // and its response is the final authority either way.
        coupon_code: coupon?.valid ? coupon.code : undefined,
        // Loyalty points (Wave 35): sent only when the buyer
        // opted in; the server clamps to the redeemable maximum
        // and the placed order reports what was actually used.
        loyalty_points:
          useLoyalty && loyaltyPointsPreview > 0
            ? loyaltyPointsPreview
            : undefined,
        // The advance method rides along whenever the picker is
        // shown for a COD order; the server ignores it when no
        // advance is required.
        advance_method:
          method === "cod" && showAdvancePicker ? advanceMethod : undefined,
      });
      setPlaced(order);
      await refresh(); // the server cleared the cart
    } catch (err) {
      const message = err instanceof Error ? err.message : "অর্ডার করা যায়নি।";
      // Wave 17: checkout's advance 400 is the signal to show the
      // advance-method picker — the cart is intact, so the buyer
      // picks a method below and places the order again.
      if (message.toLowerCase().includes("advance")) {
        setAdvanceRequired(true);
      }
      setError(message);
    } finally {
      setPlacing(false);
    }
  }

  // DEV ONLY: settle the placed order's dev payment through the
  // API's simulate helper (the API signs and applies the callback
  // itself, through the same path as a real webhook), then reload
  // the order so the paid state shown here is the server's.
  async function onSimulateSuccess() {
    if (!token || !placed?.payment) return;
    setSimulating(true);
    setError(null);
    try {
      await devSimulateSuccess(token, placed.payment.id);
      setPlaced(await getOrder(token, placed.id));
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "সিমুলেশন করা যায়নি।"
      );
    } finally {
      setSimulating(false);
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
        <h1>চেকআউট — Checkout</h1>
        <p>
          অর্ডার করতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  if (placed) {
    return (
      <main>
        <h1>অর্ডার হয়েছে ✓ — Order placed</h1>
        <p>
          অর্ডার নম্বর: <code>{placed.id}</code>
        </p>
        <p>
          পণ্যের দাম: <PriceTag amountPoisha={placed.goods_total_poisha} />
          {placed.discount_total_poisha > 0 && (
            <>
              {" "}
              · কুপন ছাড় ({placed.coupon_code}): −
              <PriceTag amountPoisha={placed.discount_total_poisha} />
            </>
          )}
          {placed.loyalty_discount_poisha > 0 && (
            <>
              {" "}
              · লয়্যালটি ছাড় ({placed.loyalty_points_used} পয়েন্ট): −
              <PriceTag amountPoisha={placed.loyalty_discount_poisha} />
            </>
          )}{" "}
          · ডেলিভারি: <PriceTag amountPoisha={placed.delivery_total_poisha} /> ·
          সর্বমোট:{" "}
          <strong>
            <PriceTag amountPoisha={placed.grand_total_poisha} />
          </strong>
        </p>
        <p>
          পেমেন্ট: {METHOD_LABELS[placed.payment_method] ?? placed.payment_method}{" "}
          — অবস্থা:{" "}
          <strong>{paymentStatusLabel(placed.payment_status)}</strong>
          {placed.payment_status === "pay_on_delivery" &&
            " (ডেলিভারির সময় পরিশোধ করতে হবে)"}
        </p>
        {placed.advance_required_poisha > 0 && (
          <p>
            অগ্রিম:{" "}
            <PriceTag amountPoisha={placed.advance_required_poisha} />
            {placed.advance_paid_poisha > 0 ? (
              <>
                {" "}
                — পরিশোধিত:{" "}
                <PriceTag amountPoisha={placed.advance_paid_poisha} /> ·
                ডেলিভারিতে বাকি:{" "}
                <strong>
                  <PriceTag
                    amountPoisha={
                      placed.grand_total_poisha - placed.advance_paid_poisha
                    }
                  />
                </strong>
              </>
            ) : placed.payment_status === "pay_on_delivery" ? (
              " — কোনো পেমেন্ট গেটওয়ে চালু না থাকায় অগ্রিম নেওয়া হয়নি; পুরো টাকা ডেলিভারির সময় দিতে হবে। (No gateway is live yet, so no advance was collected.)"
            ) : (
              " — এখনো পরিশোধ হয়নি।"
            )}
          </p>
        )}
        {placed.payment_status === "advance_pending" && (
          <article className="card">
            <h3>অগ্রিম পরিশোধ করুন — Pay the advance</h3>
            <p>
              এই অর্ডারটি পাঠানোর আগে অগ্রিম{" "}
              <PriceTag amountPoisha={placed.advance_required_poisha} />{" "}
              পরিশোধ করতে হবে
              {placed.payment
                ? ` (${METHOD_LABELS[placed.payment.method] ?? placed.payment.method} দিয়ে)`
                : ""}
              । অগ্রিম পরিশোধ হলেই বিক্রেতা কুরিয়ার বুক করতে পারবেন;
              বাকি টাকা ডেলিভারির সময় দিতে হবে।
            </p>
          </article>
        )}
        {error && <p role="alert">{error}</p>}
        {(placed.payment_status === "pending" ||
          placed.payment_status === "advance_pending") &&
          placed.payment?.dev_redirect_url && (
            <article className="card">
              <h3>ডেভ মোড (পরীক্ষামূলক) — DEV payment simulator</h3>
              <p>
                এটি একটি সিমুলেটেড পেমেন্ট — আসল টাকা লেনদেন হয়নি।
                (This is a simulated payment; no real money moved.)
              </p>
              <p>
                <button
                  type="button"
                  disabled={simulating}
                  onClick={() => void onSimulateSuccess()}
                >
                  {simulating
                    ? "সিমুলেশন হচ্ছে…"
                    : "Simulate payment success — পেমেন্ট সফল সিমুলেট করুন"}
                </button>
              </p>
            </article>
          )}
        {placed.sub_orders.map((sub) => (
          <article key={sub.id} className="card">
            <h3>{sub.store_slug}</h3>
            {sub.items.map((item) => (
              <p key={item.id}>
                {item.title_snapshot} × {item.quantity} —{" "}
                <PriceTag amountPoisha={item.line_total_poisha} />
              </p>
            ))}
            <p>
              ডেলিভারি চার্জ:{" "}
              <PriceTag amountPoisha={sub.delivery_charge_poisha} />
            </p>
          </article>
        ))}
        <p>
          <Link href="/orders">আমার অর্ডার দেখুন — My orders</Link>
          {" · "}
          <Link href={`/orders/${placed.id}`}>রসিদ দেখুন — View receipt</Link>
          {" · "}
          <Link href="/track">অর্ডার ট্র্যাক করুন — Track order</Link>{" "}
          (অর্ডার নম্বর ও মোবাইল নম্বর দিয়ে, লগইন ছাড়াই)
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>চেকআউট — Checkout</h1>
      {error && <p role="alert">{error}</p>}
      {loading && items.length === 0 ? (
        <p>কার্ট লোড হচ্ছে…</p>
      ) : items.length === 0 ? (
        <p>
          কার্ট খালি — আগে পণ্য যোগ করুন। <Link href="/">পণ্য দেখুন</Link>
        </p>
      ) : (
        <>
          <h2>বিক্রেতা অনুযায়ী হিসাব — Per-seller breakdown</h2>
          {groups.map((group) => (
            <article key={group.storeSlug} className="card">
              <h3>{group.storeSlug}</h3>
              {group.items.map((item) => (
                <p key={item.product_id}>
                  {item.title_bn ?? item.title} × {item.quantity} —{" "}
                  <PriceTag amountPoisha={item.line_total_poisha} />
                </p>
              ))}
              <p>
                পণ্য: <PriceTag amountPoisha={group.goodsPoisha} /> · ডেলিভারি
                (আনুমানিক):{" "}
                {charges[group.storeSlug] != null ? (
                  <PriceTag amountPoisha={charges[group.storeSlug] ?? 0} />
                ) : (
                  "হিসাব করা যায়নি"
                )}
              </p>
            </article>
          ))}
          <p>
            পণ্যের মোট: <PriceTag amountPoisha={goodsTotalPoisha} />
            {previewKnown && (
              <>
                {" "}
                · ডেলিভারি (আনুমানিক):{" "}
                <PriceTag amountPoisha={previewDelivery} /> · সর্বমোট
                (আনুমানিক):{" "}
                <PriceTag amountPoisha={goodsTotalPoisha + previewDelivery} />
              </>
            )}
          </p>
          <p>
            আনুমানিক হিসাব শুধু দেখার জন্য — অর্ডারের সময় সার্ভারের হিসাবই
            চূড়ান্ত। (Preview only; the server's totals at order time are
            final.)
          </p>

          <h2>কুপন — Coupon</h2>
          <p>
            <label>
              কুপন কোড
              <input
                value={couponCode}
                onChange={(e) => {
                  setCouponCode(e.target.value);
                  setCoupon(null);
                }}
                placeholder="EID10"
              />
            </label>{" "}
            <button
              type="button"
              disabled={applyingCoupon || !couponCode.trim()}
              onClick={() => void onApplyCoupon()}
            >
              {applyingCoupon ? "যাচাই হচ্ছে…" : "কুপন প্রয়োগ করুন"}
            </button>
          </p>
          {coupon &&
            (coupon.valid ? (
              <p>
                কুপন ছাড়: −<PriceTag amountPoisha={coupon.discount_poisha} />
                {previewKnown && (
                  <>
                    {" "}
                    · নতুন আনুমানিক সর্বমোট:{" "}
                    <PriceTag
                      amountPoisha={
                        goodsTotalPoisha - coupon.discount_poisha + previewDelivery
                      }
                    />
                  </>
                )}
              </p>
            ) : (
              <p role="alert">
                কুপনটি ব্যবহার করা যাচ্ছে না
                {coupon.reason ? `: ${coupon.reason}` : ""}
              </p>
            ))}

          {loyalty && loyalty.available_points > 0 && (
            <>
              <h2>লয়্যালটি পয়েন্ট — Loyalty points</h2>
              <p>
                আপনার কাছে {loyalty.available_points} পয়েন্ট আছে (১
                পয়েন্ট ={" "}
                <PriceTag amountPoisha={loyalty.point_value_poisha} />
                )। এই অর্ডারে সর্বোচ্চ {loyaltyMaxUsable} পয়েন্ট
                ব্যবহার করা যাবে — ছাড় পণ্যের মোট দামের (কুপনের পর)
                অর্ধেকের বেশি হয় না। (You have {loyalty.available_points}{" "}
                points; at most {loyaltyMaxUsable} can be used on this
                order.)
              </p>
              <p>
                <label>
                  <input
                    type="checkbox"
                    checked={useLoyalty}
                    onChange={(e) => setUseLoyalty(e.target.checked)}
                  />{" "}
                  পয়েন্ট ব্যবহার করুন — Use points
                </label>
              </p>
              {useLoyalty && (
                <p>
                  <label>
                    কত পয়েন্ট ব্যবহার করবেন (খালি রাখলে সর্বোচ্চটাই
                    ব্যবহৃত হবে)
                    <input
                      value={loyaltyPointsInput}
                      onChange={(e) => setLoyaltyPointsInput(e.target.value)}
                      placeholder={String(loyaltyMaxUsable)}
                      inputMode="numeric"
                    />
                  </label>{" "}
                  ছাড়: −
                  <PriceTag amountPoisha={loyaltyDiscountPreview} /> (
                  {loyaltyPointsPreview} পয়েন্ট)
                </p>
              )}
            </>
          )}

          <h2>ডেলিভারির তথ্য — Delivery details</h2>
          <form onSubmit={onPlaceOrder}>
            <label>
              আপনার নাম
              <input
                value={name}
                onChange={(e) => setName(e.target.value)}
                required
              />
            </label>
            <label>
              মোবাইল নম্বর (01XXXXXXXXX)
              <input
                value={phone}
                onChange={(e) => setPhone(e.target.value)}
                placeholder="01712345678"
                inputMode="tel"
                required
              />
            </label>
            <label>
              ঠিকানা
              <input
                value={address}
                onChange={(e) => setAddress(e.target.value)}
                required
              />
            </label>
            <label>
              এলাকা (Zone)
              <select
                value={zone.value}
                onChange={(e) =>
                  setZone(ZONES.find((z) => z.value === e.target.value) ?? ZONES[0])
                }
              >
                {ZONES.map((z) => (
                  <option key={z.value} value={z.value}>
                    {z.label}
                  </option>
                ))}
              </select>
            </label>

            <h2>পেমেন্ট — Payment</h2>
            {methods === null ? (
              <p>পেমেন্ট পদ্ধতি লোড হচ্ছে…</p>
            ) : (
              methods.map((info) => (
                <label key={info.method}>
                  <input
                    type="radio"
                    name="payment-method"
                    value={info.method}
                    disabled={!info.available}
                    checked={method === info.method}
                    onChange={() => setMethod(info.method)}
                  />{" "}
                  {METHOD_LABELS[info.method] ?? info.method}
                  {!info.available && " — এখনো চালু হয়নি"}
                  {info.available &&
                    info.mode === "dev" &&
                    " — ডেভ মোড (পরীক্ষামূলক)"}
                </label>
              ))
            )}

            {method === "cod" &&
              policy !== null &&
              policy.threshold_poisha > 0 &&
              previewKnown &&
              previewGrand >= policy.threshold_poisha && (
                <p>
                  এই অর্ডারের মোট{" "}
                  <PriceTag amountPoisha={policy.threshold_poisha} /> বা তার
                  বেশি — COD অর্ডারে {policy.percent_bps / 100}% অগ্রিম
                  লাগতে পারে। সার্ভারই চূড়ান্ত সিদ্ধান্ত নেয় (আপনার
                  আগের অর্ডারের ইতিহাসও দেখা হয়)। (Orders at or above
                  the threshold may need a {policy.percent_bps / 100}%
                  advance; the server decides.)
                </p>
              )}
            {showAdvancePicker && (
              <>
                <h3>অগ্রিম পেমেন্টের মাধ্যম — Advance payment method</h3>
                <p>
                  এই অর্ডারে অগ্রিম লাগবে — কোন মাধ্যমে অগ্রিম দেবেন
                  বেছে নিন। অগ্রিম ছাড়া অর্ডার পাঠানো যাবে না।
                </p>
                {(methods ?? [])
                  .filter((info) => info.method !== "cod")
                  .map((info) => (
                    <label key={info.method}>
                      <input
                        type="radio"
                        name="advance-method"
                        value={info.method}
                        disabled={!info.available}
                        checked={advanceMethod === info.method}
                        onChange={() => setAdvanceMethod(info.method)}
                      />{" "}
                      {METHOD_LABELS[info.method] ?? info.method}
                      {!info.available && " — এখনো চালু হয়নি"}
                      {info.available &&
                        info.mode === "dev" &&
                        " — ডেভ মোড (পরীক্ষামূলক)"}
                    </label>
                  ))}
              </>
            )}

            <p>
              <button type="submit" disabled={placing}>
                {placing ? "অর্ডার হচ্ছে…" : "অর্ডার করুন — Place order"}
              </button>
            </p>
          </form>
        </>
      )}
    </main>
  );
}
