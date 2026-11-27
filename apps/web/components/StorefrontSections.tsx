"use client";

import { useEffect, useState } from "react";
import ProductRail from "./ProductRail";
import { searchProducts, type Product } from "@/lib/api";

/**
 * A storefront's merchandising sections (Wave 52): above the
 * store's full catalogue grid, the store gets the same
 * section grammar as the marketplace home — an on-sale rail
 * (products with a live flash price, or a compare-at above
 * the price), a best-sellers rail (real delivered sales
 * only), and a new-arrivals rail — all scoped to this store
 * and all derived from ONE search read of the store's
 * products, sorted here by the shared view's own fields.
 *
 * Sections with nothing honest to show render nothing (the
 * rails' own rule), and a failed read renders nothing at
 * all: the catalogue grid below is the storefront's content
 * and its fallback, exactly like the home page's.
 */
export default function StorefrontSections({ slug }: { slug: string }) {
  const [products, setProducts] = useState<Product[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    searchProducts({ store_slug: slug, per_page: 50 })
      .then((page) => {
        if (!cancelled) setProducts(page.items);
      })
      .catch(() => {
        if (!cancelled) setProducts([]);
      });
    return () => {
      cancelled = true;
    };
  }, [slug]);

  if (products === null || products.length === 0) return null;

  const onSale = products
    .filter(
      (p) =>
        p.flash_price_poisha !== null ||
        (p.compare_at_poisha !== null && p.compare_at_poisha > p.price_poisha)
    )
    .slice(0, 10);
  const bestSellers = products
    .filter((p) => p.sold_count > 0)
    .sort((a, b) => b.sold_count - a.sold_count)
    .slice(0, 10);
  const newArrivals = [...products]
    .sort((a, b) => b.created_at.localeCompare(a.created_at))
    .slice(0, 10);

  return (
    <>
      <ProductRail title="অফারে আছে — On sale at this store" products={onSale} />
      <ProductRail
        title="এই স্টোরের সেরা বিক্রিত — This store's best sellers"
        products={bestSellers}
      />
      <ProductRail title="নতুন এসেছে — New at this store" products={newArrivals} />
    </>
  );
}
