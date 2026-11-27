"use client";

import Link from "next/link";
import DeliveryEstimate from "@/components/DeliveryEstimate";
import PriceTag from "@/components/PriceTag";
import ProductImage from "@/components/ProductImage";
import { Badge } from "@/components/ui";
import { formatBdt } from "@/lib/format";
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
 */
export default function CartPage() {
  const { ready, token } = useAuth();
  const { items, goodsTotalPoisha, count, loading, error, setQuantity, remove, clear } =
    useCart();
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
      {loading && items.length === 0 ? (
        <p>কার্ট লোড হচ্ছে…</p>
      ) : items.length === 0 ? (
        <p>
          কার্ট খালি। <Link href="/">পণ্য দেখুন — browse products</Link>
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
    </main>
  );
}
