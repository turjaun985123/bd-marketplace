"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import ProductCard from "@/components/ProductCard";
import { EmptyState, SectionHeader } from "@/components/ui";
import {
  getCollection,
  getHomeFeed,
  recordMerchEvent,
  type MerchCollection,
  type MerchCollectionDetail,
} from "@/lib/api";

/**
 * A collection's public landing page (Wave 52): the
 * admin-curated set as its own page — header, then the
 * products in the admin's chosen membership order (the API
 * serves them ordered; this page never re-sorts). An
 * unpublished or unknown slug is the API's 404, rendered
 * here as an honest not-found with the way back.
 *
 * Viewing the page records the collection_view
 * merchandising event once per mount (fire-and-forget).
 */
export default function CollectionPage({
  params,
}: {
  params: { slug: string };
}) {
  const [detail, setDetail] = useState<MerchCollectionDetail | null>(null);
  const [missing, setMissing] = useState(false);
  const [others, setOthers] = useState<MerchCollection[]>([]);

  useEffect(() => {
    let cancelled = false;
    getCollection(params.slug)
      .then((data) => {
        if (cancelled) return;
        setDetail(data);
        recordMerchEvent("collection_view", data.id);
      })
      .catch(() => {
        if (!cancelled) setMissing(true);
      });
    // The other live collections, for the cross-links at
    // the foot of the page (the home feed's sections are
    // the platform's published set).
    getHomeFeed()
      .then((feed) => {
        if (!cancelled) {
          setOthers(
            feed.collections.filter((c) => c.slug !== params.slug)
          );
        }
      })
      .catch(() => {
        // Cross-links are garnish; their absence is silent.
      });
    return () => {
      cancelled = true;
    };
  }, [params.slug]);

  if (missing) {
    return (
      <main>
        <EmptyState title="কালেকশনটি পাওয়া যায়নি — Collection not found">
          <p>
            এই কালেকশনটি এখন প্রকাশিত নয়, অথবা লিংকটি ভুল। (This
            collection is not published right now, or the link is
            wrong.)
          </p>
          <p>
            <Link href="/">মার্কেটপ্লেসে ফিরুন — Back to the marketplace</Link>
          </p>
        </EmptyState>
      </main>
    );
  }

  if (detail === null) {
    return (
      <main>
        <p>লোড হচ্ছে… (Loading…)</p>
      </main>
    );
  }

  return (
    <main>
      <header className="collection-header">
        <h1>{detail.title}</h1>
        {detail.subtitle ? <p>{detail.subtitle}</p> : null}
        <p>
          {detail.products.length}টি পণ্য — products ·{" "}
          <Link href="/">মার্কেটপ্লেস — Marketplace</Link>
        </p>
      </header>
      {detail.products.length === 0 ? (
        <EmptyState title="এই কালেকশনে এখনো পণ্য নেই — No products yet">
          <p>
            কালেকশনটি তৈরি, কিন্তু এখনো কোনো পণ্য যোগ হয়নি। (The
            collection exists; no products have been added yet.)
          </p>
        </EmptyState>
      ) : (
        <div className="collection-grid">
          {detail.products.map((p) => (
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
              imageUrl={p.image_url}
              hoverImageUrl={p.hover_image_url}
              compareAtPoisha={p.compare_at_poisha}
              soldCount={p.sold_count}
              flashPricePoisha={p.flash_price_poisha}
              flashEndsAt={p.flash_ends_at}
            />
          ))}
        </div>
      )}
      {others.length > 0 && (
        <section>
          <SectionHeader title="আরও কালেকশন — More collections" />
          <p>
            {others.map((collection, i) => (
              <span key={collection.id}>
                {i > 0 ? " · " : ""}
                <Link href={`/collections/${collection.slug}`}>
                  {collection.title}
                </Link>
              </span>
            ))}
          </p>
        </section>
      )}
    </main>
  );
}
