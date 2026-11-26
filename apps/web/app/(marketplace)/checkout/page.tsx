"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import { placeOrder, type CartItem, type Order } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";
import { useCart } from "@/hooks/useCart";
import {
  useDeliveryCharge,
  type DeliveryZone,
} from "@/hooks/useDeliveryCharge";

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
 * Checkout (COD only). The per-seller delivery preview is display-only,
 * quoted through the shared useDeliveryCharge hook (one quote per
 * seller group, billed on the group's real weight in grams — the
 * server's Wave 4 rule). The server's totals in the place-order
 * response are the final truth and are what the success screen shows.
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

  const previewDelivery = groups.reduce(
    (sum, g) => sum + (charges[g.storeSlug] ?? 0),
    0
  );
  const previewKnown = groups.every((g) => charges[g.storeSlug] != null);

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
        payment_method: "cod",
      });
      setPlaced(order);
      await refresh(); // the server cleared the cart
    } catch (err) {
      setError(err instanceof Error ? err.message : "অর্ডার করা যায়নি।");
    } finally {
      setPlacing(false);
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
          পণ্যের দাম: <PriceTag amountPoisha={placed.goods_total_poisha} /> ·
          ডেলিভারি: <PriceTag amountPoisha={placed.delivery_total_poisha} /> ·
          সর্বমোট (ডেলিভারির সময় পরিশোধ):{" "}
          <strong>
            <PriceTag amountPoisha={placed.grand_total_poisha} />
          </strong>
        </p>
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
            <label>
              <input type="radio" checked readOnly /> ক্যাশ অন ডেলিভারি (COD) —
              পণ্য হাতে পেয়ে টাকা দিন
            </label>
            <label>
              <input type="radio" disabled /> বিকাশ / নগদ / SSLCommerz — শীঘ্রই
              আসছে
            </label>

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
