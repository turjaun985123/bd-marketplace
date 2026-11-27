"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import ProductCard from "./ProductCard";
import { getStoreProducts, listProducts, type Product } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";
import { useCart } from "@/hooks/useCart";

/**
 * Shared product grid: fetches real products from the Rust API via the
 * typed client and renders them with the shared ProductCard. Used by
 * the central marketplace (no `storeSlug`) and by storefront pages
 * (with `storeSlug`) — one grid, two listings, never a copy.
 * Handles the three honest states — loading, API unreachable, and
 * "no products yet" — in Bangla first.
 *
 * ProductCard stays presentational: the add-to-cart button is wired
 * here, in the grid, through the shared useCart hook.
 *
 * Wave 18: `layout` picks the storefront theme's presentation —
 * "grid" (default; the exact markup every existing page renders),
 * "list" (minimal theme: compact rows) or "large" (bold theme:
 * two-column oversized cards). Data, cart wiring and states are
 * identical in all three; only wrapper/card classes change.
 */
export default function ProductGrid({
  storeSlug,
  layout = "grid",
}: {
  storeSlug?: string;
  layout?: "grid" | "list" | "large";
}) {
  const { token } = useAuth();
  const { add, error: cartError } = useCart();
  const [products, setProducts] = useState<Product[] | null>(null);
  const [offline, setOffline] = useState(false);
  const [addedId, setAddedId] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const request = storeSlug
      ? getStoreProducts(storeSlug, token)
      : listProducts({ limit: 24 });
    request
      .then((list) => {
        if (!cancelled) setProducts(list.items);
      })
      .catch(() => {
        if (!cancelled) setOffline(true);
      });
    return () => {
      cancelled = true;
    };
  }, [storeSlug, token]);

  async function onAdd(productId: string) {
    setAddedId(null);
    await add(productId, 1);
    setAddedId(productId);
  }

  if (offline) {
    return (
      <p>
        API-তে সংযোগ করা যাচ্ছে না — Rust API চালু আছে কিনা দেখুন। (Could not
        reach the API, so products cannot be loaded.)
      </p>
    );
  }
  if (products === null) {
    return <p>পণ্য লোড হচ্ছে… (Loading products…)</p>;
  }
  if (products.length === 0) {
    return (
      <p>
        এখনো কোনো পণ্য নেই — বিক্রেতারা পণ্য যোগ করলেই এখানে দেখা যাবে। (No
        products yet.)
      </p>
    );
  }
  return (
    <div
      className={
        layout === "list"
          ? "product-list"
          : layout === "large"
            ? "product-grid-large"
            : undefined
      }
    >
      {cartError && <p role="alert">{cartError}</p>}
      {products.map((p) => (
        <div key={p.id}>
          <ProductCard
            name={p.title_bn ?? p.title}
            storeName={p.store_slug}
            pricePoisha={p.price_poisha}
            productId={p.id}
            href={`/products/${p.id}`}
            ratingAvg={p.rating_avg}
            ratingCount={p.rating_count}
            isPromoted={p.is_promoted}
            imageUrl={p.image_url}
            hoverImageUrl={p.hover_image_url}
            compareAtPoisha={p.compare_at_poisha}
            soldCount={p.sold_count}
            flashPricePoisha={p.flash_price_poisha}
            flashEndsAt={p.flash_ends_at}
            variant={
              layout === "list" ? "row" : layout === "large" ? "large" : "card"
            }
          />
          {token ? (
            <p>
              <button type="button" onClick={() => void onAdd(p.id)}>
                কার্টে যোগ করুন — Add to cart
              </button>{" "}
              {addedId === p.id && !cartError && (
                <span>
                  যোগ হয়েছে ✓ <Link href="/cart">কার্ট দেখুন</Link>
                </span>
              )}
            </p>
          ) : (
            <p>
              <Link href="/login">কার্টে যোগ করতে লগইন করুন</Link>
            </p>
          )}
        </div>
      ))}
    </div>
  );
}
