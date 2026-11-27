"use client";

import { useEffect, useState } from "react";
import ProductCard from "./ProductCard";
import { Rail, SectionHeader } from "./ui";
import { getProduct, type Product } from "@/lib/api";
import { useRecentlyViewed } from "@/hooks/useRecentlyViewed";

/**
 * The recently-viewed rail (Wave 24's home strip, Wave
 * 53's shared component): renders the device-local
 * history as product cards, with a clear button that
 * forgets the whole history. Used on the home page and
 * on the product page (where `excludeId` hides the
 * product being viewed — "your other recently viewed").
 *
 * Wave 53 entries carry a card snapshot, so they render
 * immediately; Wave 24's bare-id entries resolve through
 * the public product endpoint and are skipped silently
 * when the product no longer resolves. The history is
 * never sent to the server (see useRecentlyViewed).
 */
export default function RecentlyViewedRail({
  title = "আপনার দেখা পণ্য — Recently viewed",
  excludeId,
}: {
  title?: string;
  excludeId?: string;
}) {
  const { entries, clear } = useRecentlyViewed();
  const [resolved, setResolved] = useState<Record<string, Product>>({});

  const visible = entries.filter((entry) => entry.id !== excludeId);
  const legacyIds = visible
    .filter((entry) => entry.title === null && !resolved[entry.id])
    .map((entry) => entry.id);

  useEffect(() => {
    if (legacyIds.length === 0) return;
    let cancelled = false;
    Promise.all(
      legacyIds.map((id) => getProduct(id).catch(() => null))
    ).then((products) => {
      if (cancelled) return;
      setResolved((current) => {
        const next = { ...current };
        products.forEach((product, index) => {
          if (product) next[legacyIds[index]] = product;
        });
        return next;
      });
    });
    return () => {
      cancelled = true;
    };
    // legacyIds is derived from entries + resolved; the
    // join is its canonical form for the effect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [legacyIds.join(",")]);

  const cards = visible.flatMap((entry) => {
    if (entry.title !== null && entry.price_poisha !== null) {
      return [
        {
          id: entry.id,
          name: entry.title,
          storeName: entry.store_slug ?? "",
          pricePoisha: entry.price_poisha,
          flashPricePoisha: entry.flash_price_poisha,
          imageUrl: entry.image_url,
        },
      ];
    }
    const product = resolved[entry.id];
    if (!product) return [];
    return [
      {
        id: product.id,
        name: product.title_bn ?? product.title,
        storeName: product.store_slug,
        pricePoisha: product.price_poisha,
        flashPricePoisha: product.flash_price_poisha,
        imageUrl: product.image_url,
      },
    ];
  });

  if (cards.length === 0) return null;

  return (
    <section>
      <div className="recently-viewed-head">
        <SectionHeader title={title} />
        <button type="button" className="recently-viewed-clear" onClick={clear}>
          মুছে ফেলুন — Clear
        </button>
      </div>
      <Rail cards>
        {cards.map((card) => (
          <ProductCard
            key={card.id}
            productId={card.id}
            name={card.name}
            storeName={card.storeName}
            pricePoisha={card.pricePoisha}
            href={`/products/${card.id}`}
            imageUrl={card.imageUrl}
            flashPricePoisha={card.flashPricePoisha}
          />
        ))}
      </Rail>
    </section>
  );
}
