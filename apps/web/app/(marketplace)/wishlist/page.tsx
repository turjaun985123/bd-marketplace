"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import ProductCard from "@/components/ProductCard";
import { getWishlist, type Product } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";
import { useWishlist } from "@/hooks/useWishlist";

/**
 * The wishlist page (Wave 24): the signed-in user's
 * server-side saved-for-later list, newest first. The list
 * lives on the account, so it is the same on every device.
 * Unsaving from a card's heart refreshes the list (the
 * shared ids set changing is the refetch trigger), so a
 * removed product leaves the page without a manual reload.
 */
export default function WishlistPage() {
  const { ready, token } = useAuth();
  const { ids } = useWishlist();
  const [items, setItems] = useState<Product[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    if (!token) return;
    try {
      const list = await getWishlist(token);
      setItems(list.items);
      setError(null);
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "উইশলিস্ট লোড করা যায়নি।"
      );
    }
  }, [token]);

  useEffect(() => {
    if (!ready || !token) return;
    let cancelled = false;
    reload().catch(() => {
      if (!cancelled) setError("উইশলিস্ট লোড করা যায়নি।");
    });
    return () => {
      cancelled = true;
    };
    // `ids` is a dependency on purpose: a heart toggle
    // anywhere (including on this page) refetches the list.
  }, [ready, token, reload, ids]);

  if (!ready) {
    return (
      <main>
        <p>লোড হচ্ছে… (Loading…)</p>
      </main>
    );
  }

  if (!token) {
    return (
      <main>
        <h1>আমার উইশলিস্ট — My wishlist</h1>
        <p>
          উইশলিস্ট দেখতে <Link href="/login">লগইন করুন</Link>। পছন্দের
          পণ্য সেভ করে রাখলে যেকোনো ডিভাইস থেকে এখানে পাবেন।
        </p>
        <p>
          <Link href="/">← মার্কেটপ্লেস</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <p>
        <Link href="/">← মার্কেটপ্লেস</Link>
      </p>
      <h1>আমার উইশলিস্ট — My wishlist</h1>
      {error && <p role="alert">{error}</p>}
      {items === null && !error ? (
        <p>লোড হচ্ছে… (Loading…)</p>
      ) : items !== null && items.length === 0 ? (
        <p>
          আপনার উইশলিস্ট খালি — পছন্দের পণ্যে ♡ চাপ দিলেই এখানে জমা
          হবে। (Your wishlist is empty.)
        </p>
      ) : (
        <div>
          {items?.map((p) => (
            <ProductCard
              key={p.id}
              productId={p.id}
              name={p.title_bn ?? p.title}
              storeName={p.store_slug}
              pricePoisha={p.price_poisha}
              href={`/products/${p.id}`}
              ratingAvg={p.rating_avg}
              ratingCount={p.rating_count}
              isPromoted={p.is_promoted}
            />
          ))}
        </div>
      )}
    </main>
  );
}
