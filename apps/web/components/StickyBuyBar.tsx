"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "./PriceTag";
import ProductImage from "./ProductImage";

/**
 * The sticky buy bar (Wave 54): once the buyer scrolls
 * past the product page's main buy block, a slim bar
 * pins to the bottom of the viewport with the product's
 * thumbnail, its CURRENT price (the same effective price
 * the page shows — flash-aware, variant-aware, computed
 * by the page and passed down) and the two purchase
 * actions. It renders nothing until the scroll threshold
 * passes, and nothing at all for a signed-out visitor
 * beyond the login link — the actions it offers are the
 * page's own add / buy-now handlers, so the bar can never
 * diverge from the buy block's behaviour (variant pick,
 * stock state and cart errors all live in the page).
 */
export default function StickyBuyBar({
  title,
  imageUrl,
  pricePoisha,
  wasPoisha,
  canBuy,
  signedIn,
  added,
  onAdd,
  onBuyNow,
}: {
  title: string;
  imageUrl: string | null;
  pricePoisha: number;
  /** The struck-through comparison price, when one applies. */
  wasPoisha: number | null;
  canBuy: boolean;
  signedIn: boolean;
  added: boolean;
  onAdd: () => void;
  onBuyNow: () => void;
}) {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    function onScroll() {
      setVisible(window.scrollY > 600);
    }
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, []);

  if (!visible) return null;

  return (
    <div className="sticky-buy-bar" role="region" aria-label="কিনুন — Buy">
      {imageUrl && (
        <ProductImage
          src={imageUrl}
          alt=""
          className="sticky-buy-bar-thumb"
        />
      )}
      <span className="sticky-buy-bar-title">{title}</span>
      <span className="sticky-buy-bar-price">
        <PriceTag amountPoisha={pricePoisha} />{" "}
        {wasPoisha !== null && wasPoisha > pricePoisha && (
          <s>
            <PriceTag amountPoisha={wasPoisha} />
          </s>
        )}
      </span>
      {signedIn ? (
        <span className="sticky-buy-bar-actions">
          <button type="button" disabled={!canBuy} onClick={onAdd}>
            কার্টে যোগ করুন
          </button>{" "}
          <button type="button" disabled={!canBuy} onClick={onBuyNow}>
            এখনই কিনুন — Buy now
          </button>{" "}
          {added && <Link href="/cart">কার্ট দেখুন ✓</Link>}
        </span>
      ) : (
        <Link href="/login">কিনতে লগইন করুন</Link>
      )}
    </div>
  );
}
