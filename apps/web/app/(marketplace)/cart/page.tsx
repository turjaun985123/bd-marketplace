"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import DeliveryEstimate from "@/components/DeliveryEstimate";
import PriceTag from "@/components/PriceTag";
import ProductImage from "@/components/ProductImage";
import { Badge } from "@/components/ui";
import { formatBdt } from "@/lib/format";
import { getBuyAgain, type BuyAgainItem } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";
import { useCart } from "@/hooks/useCart";

/**
 * The buyer's cart page (mobile-first): line items with quantity
 * steppers and remove buttons, goods total, and a link to checkout.
 * All cart state comes from the shared useCart hook — delivery charges
 * are quoted per seller at checkout, not here.
 *
 * Wave 52: a line priced by a live flash sale says so — the
 * sale badge, the struck pre-sale price beside the unit price,
 * and a savings total under the goods total. The saving is
 * computed from the API's two unit prices per line
 * (`list_unit_price_poisha` − `unit_price_poisha`) × quantity:
 * the same numbers checkout will snapshot, never a sticker
 * invented for the cart.
 *
 * Wave 55: every line can be parked on the save-for-later
 * shelf ("পরে কিনুন"), and the shelf renders below the cart
 * with move-back and remove actions. Saved lines are not in
 * the goods total, never check out, and survive "clear
 * cart" — the API's rules, mirrored here by rendering the
 * two arrays the cart response carries separately. Quantity
 * steppers still go through the one endpoint; past stock
 * the server answers its bilingual 400 and the hook's error
 * line shows it — the displayed quantity stays the server's.
 */
export default function CartPage() {
  const { ready, token } = useAuth();
  const {
    items,
    savedItems,
    goodsTotalPoisha,
    count,
    loading,
    error,
    add,
    setQuantity,
    remove,
    clear,
    moveToSaved,
    moveToCart,
    moveAllSaved,
    clearSaved,
  } = useCart();
  // The buy-again rail (Wave 55): past-order products
  // that can be bought again today, from the API — the
  // rail hides itself entirely when there is nothing to
  // offer (a failed load reads the same as an empty rail;
  // the cart itself never depends on it).
  const [buyAgain, setBuyAgain] = useState<BuyAgainItem[]>([]);
  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getBuyAgain(token)
      .then((rail) => {
        if (!cancelled) setBuyAgain(rail.items);
      })
      .catch(() => {
        if (!cancelled) setBuyAgain([]);
      });
    return () => {
      cancelled = true;
    };
  }, [token, items.length]);
  const savingsPoisha = items.reduce(
    (sum, item) =>
      sum +
      Math.max(0, item.list_unit_price_poisha - item.unit_price_poisha) *
        item.quantity,
    0
  );

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
      {loading && items.length === 0 && savedItems.length === 0 ? (
        <p>কার্ট লোড হচ্ছে…</p>
      ) : items.length === 0 && savedItems.length === 0 ? (
        <p>
          কার্ট খালি। <Link href="/">পণ্য দেখুন — browse products</Link>
        </p>
      ) : items.length === 0 ? (
        <p>
          কার্ট খালি — তবে পরে কেনার জন্য রাখা পণ্য নিচে আছে। (The cart
          is empty; your saved-for-later items are below.)
        </p>
      ) : (
        <>
          {items.map((item) => (
            <article
              key={`${item.product_id}:${item.variant_id ?? ""}`}
              className="card"
            >
              {/* Wave 51: the line's product thumbnail. */}
              <ProductImage
                src={item.image_url}
                alt={item.title_bn ?? item.title}
                placeholderText={item.title_bn ?? item.title}
                className="cart-item-image"
              />
              <h3>
                {item.title_bn ?? item.title}
                {item.variant_name && <> — {item.variant_name}</>}
              </h3>
              <p>
                {item.store_slug} · একক দাম{" "}
                <PriceTag amountPoisha={item.unit_price_poisha} />{" "}
                {item.list_unit_price_poisha > item.unit_price_poisha && (
                  <>
                    <s>
                      <PriceTag amountPoisha={item.list_unit_price_poisha} />
                    </s>{" "}
                    <Badge tone="flash">ফ্ল্যাশ সেল — Flash sale</Badge>
                  </>
                )}
              </p>
              <p>
                পরিমাণ:{" "}
                <button
                  type="button"
                  aria-label="পরিমাণ কমান"
                  onClick={() =>
                    void setQuantity(
                      item.product_id,
                      item.quantity - 1,
                      item.variant_id
                    )
                  }
                >
                  −
                </button>{" "}
                {item.quantity}{" "}
                <button
                  type="button"
                  aria-label="পরিমাণ বাড়ান"
                  onClick={() =>
                    void setQuantity(
                      item.product_id,
                      item.quantity + 1,
                      item.variant_id
                    )
                  }
                >
                  +
                </button>{" "}
                <button
                  type="button"
                  onClick={() => void remove(item.product_id, item.variant_id)}
                >
                  মুছে ফেলুন — Remove
                </button>{" "}
                <button
                  type="button"
                  onClick={() =>
                    void moveToSaved(item.product_id, item.variant_id)
                  }
                >
                  পরে কিনুন — Save for later
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
          {savingsPoisha > 0 && (
            <p>
              ফ্ল্যাশ সেলে আপনার সাশ্রয় — You save{" "}
              <strong>{formatBdt(savingsPoisha)}</strong> on this cart
            </p>
          )}
          <p>
            ডেলিভারি চার্জ চেকআউটে বিক্রেতা অনুযায়ী হিসাব হবে। (Delivery is
            charged per seller at checkout.)
          </p>
          {/* Wave 54 extension: the same estimate block as
              the product page, over the cart's TOTAL parcel
              weight — an orientation figure; checkout still
              splits the charge per seller. */}
          <DeliveryEstimate
            weightGrams={items.reduce(
              (sum, item) => sum + item.weight_grams * item.quantity,
              0
            )}
          />
          <p>
            <Link href="/checkout">চেকআউটে যান — Checkout</Link>
            {" · "}
            <button type="button" onClick={() => void clear()}>
              কার্ট খালি করুন — Clear cart
            </button>
          </p>
        </>
      )}
      {savedItems.length > 0 && (
        <>
          <h2>পরে কেনার জন্য রাখা — Saved for later</h2>
          <p>
            এই পণ্যগুলো মোট দামে ধরা নেই এবং চেকআউটেও যাবে না —
            কার্টে ফেরালে তবেই কেনা যাবে। (Saved items are not in
            the total and do not check out.)
          </p>
          <p>
            <button type="button" onClick={() => void moveAllSaved()}>
              সবগুলো কার্টে ফেরান — Move all to cart
            </button>{" "}
            <button type="button" onClick={() => void clearSaved()}>
              তাক খালি করুন — Clear the shelf
            </button>
          </p>
          {savedItems.map((item) => (
            <article
              key={`${item.product_id}:${item.variant_id ?? ""}`}
              className="card"
            >
              <ProductImage
                src={item.image_url}
                alt={item.title_bn ?? item.title}
                placeholderText={item.title_bn ?? item.title}
                className="cart-item-image"
              />
              <h3>
                {item.title_bn ?? item.title}
                {item.variant_name && <> — {item.variant_name}</>}
              </h3>
              <p>
                {item.store_slug} · একক দাম{" "}
                <PriceTag amountPoisha={item.unit_price_poisha} /> · পরিমাণ{" "}
                {item.quantity}
              </p>
              <p>
                <button
                  type="button"
                  onClick={() =>
                    void moveToCart(item.product_id, item.variant_id)
                  }
                >
                  কার্টে ফেরান — Move to cart
                </button>{" "}
                <button
                  type="button"
                  onClick={() => void remove(item.product_id, item.variant_id)}
                >
                  মুছে ফেলুন — Remove
                </button>
              </p>
            </article>
          ))}
        </>
      )}
      {buyAgain.length > 0 && (
        <>
          <h2>আগে কিনেছেন — Bought before</h2>
          <p>
            আপনার আগের অর্ডারের পণ্য, আজকের দামে — যেগুলো এখনো
            কেনা যায় শুধু সেগুলোই দেখানো হচ্ছে। (From your past
            orders, at today's prices — only what can still be
            bought.)
          </p>
          {buyAgain.map((entry) => (
            <article key={entry.product_id} className="card">
              <ProductImage
                src={entry.image_url}
                alt={entry.title_bn ?? entry.title}
                placeholderText={entry.title_bn ?? entry.title}
                className="cart-item-image"
              />
              <h3>{entry.title_bn ?? entry.title}</h3>
              <p>
                {entry.store_slug} ·{" "}
                <PriceTag amountPoisha={entry.unit_price_poisha} />{" "}
                {entry.list_unit_price_poisha > entry.unit_price_poisha && (
                  <>
                    <s>
                      <PriceTag amountPoisha={entry.list_unit_price_poisha} />
                    </s>{" "}
                    <Badge tone="flash">ফ্ল্যাশ সেল — Flash sale</Badge>
                  </>
                )}{" "}
                · {entry.times_ordered} বার কেনা হয়েছে
              </p>
              <p>
                {entry.in_cart ? (
                  <>ইতিমধ্যে কার্টে আছে — Already in your cart</>
                ) : entry.has_variants ? (
                  <Link href={`/products/${entry.product_id}`}>
                    আকার/রং বেছে নিন — Choose a variant
                  </Link>
                ) : (
                  <button
                    type="button"
                    onClick={() => void add(entry.product_id, 1)}
                  >
                    কার্টে যোগ করুন — Add to cart
                  </button>
                )}
              </p>
            </article>
          ))}
        </>
      )}
    </main>
  );
}
