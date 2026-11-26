"use client";

import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import { useAuth } from "@/hooks/useAuth";
import { useCart } from "@/hooks/useCart";

/**
 * The buyer's cart page (mobile-first): line items with quantity
 * steppers and remove buttons, goods total, and a link to checkout.
 * All cart state comes from the shared useCart hook — delivery charges
 * are quoted per seller at checkout, not here.
 */
export default function CartPage() {
  const { ready, token } = useAuth();
  const { items, goodsTotalPoisha, count, loading, error, setQuantity, remove, clear } =
    useCart();

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
        <h1>আমার কার্ট — Cart</h1>
        <p>
          কার্ট দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>আমার কার্ট — Cart</h1>
      {error && <p role="alert">{error}</p>}
      {loading && items.length === 0 ? (
        <p>কার্ট লোড হচ্ছে…</p>
      ) : items.length === 0 ? (
        <p>
          কার্ট খালি। <Link href="/">পণ্য দেখুন — browse products</Link>
        </p>
      ) : (
        <>
          {items.map((item) => (
            <article key={item.product_id} className="card">
              <h3>{item.title_bn ?? item.title}</h3>
              <p>
                {item.store_slug} · একক দাম{" "}
                <PriceTag amountPoisha={item.unit_price_poisha} />
              </p>
              <p>
                পরিমাণ:{" "}
                <button
                  type="button"
                  aria-label="পরিমাণ কমান"
                  onClick={() => void setQuantity(item.product_id, item.quantity - 1)}
                >
                  −
                </button>{" "}
                {item.quantity}{" "}
                <button
                  type="button"
                  aria-label="পরিমাণ বাড়ান"
                  onClick={() => void setQuantity(item.product_id, item.quantity + 1)}
                >
                  +
                </button>{" "}
                <button
                  type="button"
                  onClick={() => void remove(item.product_id)}
                >
                  মুছে ফেলুন — Remove
                </button>
              </p>
              <p>
                মোট: <PriceTag amountPoisha={item.line_total_poisha} />
              </p>
            </article>
          ))}
          <p>
            মোট পণ্য: {count}টি · পণ্যের মোট দাম:{" "}
            <PriceTag amountPoisha={goodsTotalPoisha} />
          </p>
          <p>
            ডেলিভারি চার্জ চেকআউটে বিক্রেতা অনুযায়ী হিসাব হবে। (Delivery is
            charged per seller at checkout.)
          </p>
          <p>
            <Link href="/checkout">চেকআউটে যান — Checkout</Link>
            {" · "}
            <button type="button" onClick={() => void clear()}>
              কার্ট খালি করুন — Clear cart
            </button>
          </p>
        </>
      )}
    </main>
  );
}
