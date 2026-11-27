"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import ProductImage from "./ProductImage";
import { listStoreCollections, type StoreCollection } from "@/lib/api";

/**
 * A store's own collections on its storefront (Wave 54
 * extension): the seller's curated shelves as tiles —
 * cover (the first member's photo), title, and the count
 * of products a visitor can actually buy in it. A
 * collection whose members are all drafts has a zero
 * count and is skipped here (the shelf exists for the
 * seller, but a tile promising an empty shelf would be
 * a lie to visitors); a store with no collections
 * renders nothing.
 */
export default function StoreCollections({ slug }: { slug: string }) {
  const [collections, setCollections] = useState<StoreCollection[] | null>(
    null
  );

  useEffect(() => {
    let cancelled = false;
    listStoreCollections(slug)
      .then((list) => {
        if (!cancelled) {
          setCollections(list.items.filter((c) => c.product_count > 0));
        }
      })
      .catch(() => {
        if (!cancelled) setCollections([]);
      });
    return () => {
      cancelled = true;
    };
  }, [slug]);

  if (collections === null || collections.length === 0) return null;

  return (
    <section className="store-collections">
      <h2>স্টোরের কালেকশন — Collections</h2>
      <div className="store-collection-tiles">
        {collections.map((collection) => (
          <Link
            key={collection.id}
            className="store-collection-tile"
            href={`/${slug}/collections/${collection.slug}`}
          >
            {collection.cover_image_url && (
              <ProductImage
                src={collection.cover_image_url}
                alt=""
                className="store-collection-cover"
              />
            )}
            <span className="store-collection-title">{collection.title}</span>
            <span className="store-collection-count">
              {collection.product_count.toLocaleString("en-IN")}টি পণ্য
            </span>
          </Link>
        ))}
      </div>
    </section>
  );
}
