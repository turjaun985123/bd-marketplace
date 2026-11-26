"use client";

import { useEffect, useState } from "react";
import ProductCard from "./ProductCard";
import { listProducts, type Product } from "@/lib/api";

/**
 * Shared marketplace product grid: fetches real products from the Rust
 * API via the typed client and renders them with the shared ProductCard.
 * Handles the three honest states — loading, API unreachable, and
 * "no products yet" — in Bangla first.
 */
export default function ProductGrid() {
  const [products, setProducts] = useState<Product[] | null>(null);
  const [offline, setOffline] = useState(false);

  useEffect(() => {
    let cancelled = false;
    listProducts({ limit: 24 })
      .then((list) => {
        if (!cancelled) setProducts(list.items);
      })
      .catch(() => {
        if (!cancelled) setOffline(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

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
    <div>
      {products.map((p) => (
        <ProductCard
          key={p.id}
          name={p.title_bn ?? p.title}
          storeName={p.store_slug}
          pricePoisha={p.price_poisha}
        />
      ))}
    </div>
  );
}
