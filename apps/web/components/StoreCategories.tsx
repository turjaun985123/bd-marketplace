"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { searchProducts } from "@/lib/api";

/**
 * A store's own category tiles (Wave 54): the categories
 * THIS store's active products actually live in, with
 * their counts — read from the faceted browse's category
 * facet scoped to the store (the facet counts the store's
 * catalogue, so a tile can never advertise an empty
 * shelf). Each tile deep-links into the store's shop page
 * with that category applied. A store whose products are
 * all uncategorised renders nothing — there is no honest
 * tile to show.
 */
export default function StoreCategories({ slug }: { slug: string }) {
  const [categories, setCategories] = useState<
    { category: string; count: number }[] | null
  >(null);

  useEffect(() => {
    let cancelled = false;
    searchProducts({ stores: [slug], limit: 1 })
      .then((result) => {
        if (!cancelled) {
          setCategories(
            result.facets.categories.map((facet) => ({
              category: facet.category,
              count: facet.count,
            }))
          );
        }
      })
      .catch(() => {
        if (!cancelled) setCategories([]);
      });
    return () => {
      cancelled = true;
    };
  }, [slug]);

  if (categories === null || categories.length === 0) return null;

  return (
    <section className="store-categories">
      <h2>ক্যাটাগরি অনুযায়ী কিনুন — Shop by category</h2>
      <div className="store-category-tiles">
        {categories.map(({ category, count }) => (
          <Link
            key={category}
            className="store-category-tile"
            href={`/${slug}/shop?category=${encodeURIComponent(category)}`}
          >
            <span className="store-category-name">{category}</span>
            <span className="store-category-count">
              {count.toLocaleString("en-IN")}টি পণ্য
            </span>
          </Link>
        ))}
      </div>
    </section>
  );
}
