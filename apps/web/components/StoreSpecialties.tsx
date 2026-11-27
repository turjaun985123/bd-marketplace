"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import CategoryIcon from "./CategoryIcon";
import {
  getStoreCategories,
  type StoreCategorySpecialty,
} from "@/lib/api";

/**
 * A store's taxonomy specialisation (Wave 56): the
 * managed categories this store actually sells in, with
 * counts, from GET /v1/stores/:slug/categories — computed
 * from the store's real product links, so the block can
 * never advertise a specialisation the catalogue does not
 * back. Each tile lands on the category's IN-STORE page
 * (/<store>/categories/<slug>), keeping the shopper in
 * this store's subtree. A store with no taxonomy-linked
 * products renders nothing
 * (the Wave 54 text-label tiles in StoreCategories remain
 * the fallback vocabulary on the same page).
 */
export default function StoreSpecialties({ slug }: { slug: string }) {
  const [specialties, setSpecialties] = useState<
    StoreCategorySpecialty[] | null
  >(null);

  useEffect(() => {
    let cancelled = false;
    getStoreCategories(slug)
      .then((result) => {
        if (!cancelled) setSpecialties(result.categories);
      })
      .catch(() => {
        if (!cancelled) setSpecialties([]);
      });
    return () => {
      cancelled = true;
    };
  }, [slug]);

  if (specialties === null || specialties.length === 0) return null;

  return (
    <section className="store-specialties">
      <h2>এই স্টোরের বিশেষত্ব — This store specialises in</h2>
      <div className="store-category-tiles">
        {specialties.map((specialty) => (
          <Link
            key={specialty.id}
            className="store-category-tile"
            href={`/${slug}/categories/${specialty.slug}`}
          >
            <span className="store-category-name">
              <CategoryIcon iconKey={specialty.icon_key} />{" "}
              {specialty.name_bn ?? specialty.name_en}
            </span>
            <span className="store-category-count">
              {specialty.product_count.toLocaleString("en-IN")}টি পণ্য
            </span>
            {specialty.path.length > 1 && (
              <span className="store-category-path">
                {specialty.path
                  .slice(0, -1)
                  .map((entry) => entry.name_en)
                  .join(" › ")}
              </span>
            )}
          </Link>
        ))}
      </div>
    </section>
  );
}
