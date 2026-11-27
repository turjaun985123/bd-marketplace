"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import ProductCard from "@/components/ProductCard";
import StoreShopHeader from "@/components/StoreShopHeader";
import { getStoreCollection, type PublicStoreCollection } from "@/lib/api";

/**
 * One store collection's public page (Wave 54 extension):
 * the seller's curated shelf — its title and description
 * over its ACTIVE member products in the seller's own
 * arrangement (position order from the API, never
 * re-sorted here). Draft members stay invisible until
 * they publish, exactly like every other public listing.
 */
export default function StoreCollectionPage({
  params,
}: {
  params: { store: string; collection: string };
}) {
  const [data, setData] = useState<PublicStoreCollection | null>(null);
  const [page, setPage] = useState(1);
  const [error, setError] = useState<string | null>(null);
  const LIMIT = 24;

  const reload = useCallback(async () => {
    try {
      const result = await getStoreCollection(params.store, params.collection, {
        page,
        limit: LIMIT,
      });
      setData(result);
      setError(null);
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "কালেকশন লোড করা যায়নি।"
      );
    }
  }, [params.store, params.collection, page]);

  useEffect(() => {
    let cancelled = false;
    reload().catch(() => {
      if (!cancelled) setError("কালেকশন লোড করা যায়নি।");
    });
    return () => {
      cancelled = true;
    };
  }, [reload]);

  const pages = data ? Math.max(1, Math.ceil(data.total / LIMIT)) : 1;

  return (
    <main>
      <StoreShopHeader slug={params.store} />
      <p>
        <Link href={`/${params.store}`}>
          ← সব কালেকশন — All collections
        </Link>
      </p>
      {error && <p role="alert">{error}</p>}
      {data === null && !error ? (
        <p>লোড হচ্ছে…</p>
      ) : data ? (
        <>
          <h1>{data.title}</h1>
          {data.description && <p>{data.description}</p>}
          {data.products.length === 0 ? (
            <p>এই কালেকশনে এখন কেনার মতো পণ্য নেই।</p>
          ) : (
            <div className="search-grid">
              {data.products.map((product) => (
                <ProductCard
                  key={product.id}
                  productId={product.id}
                  name={product.title_bn ?? product.title}
                  storeName={product.store_slug}
                  pricePoisha={product.price_poisha}
                  href={`/products/${product.id}`}
                  ratingAvg={product.rating_avg}
                  ratingCount={product.rating_count}
                  isPromoted={product.is_promoted}
                  imageUrl={product.image_url}
                  hoverImageUrl={product.hover_image_url}
                  compareAtPoisha={product.compare_at_poisha}
                  soldCount={product.sold_count}
                  flashPricePoisha={product.flash_price_poisha}
                  flashEndsAt={product.flash_ends_at}
                />
              ))}
            </div>
          )}
          {pages > 1 && (
            <p>
              <button
                type="button"
                disabled={page <= 1}
                onClick={() => setPage(page - 1)}
              >
                ← আগের
              </button>{" "}
              পৃষ্ঠা {page} / {pages}{" "}
              <button
                type="button"
                disabled={page >= pages}
                onClick={() => setPage(page + 1)}
              >
                পরের →
              </button>
            </p>
          )}
        </>
      ) : null}
    </main>
  );
}
