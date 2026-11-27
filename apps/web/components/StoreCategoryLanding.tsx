"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import BrowseResults from "./BrowseResults";
import CategoryIcon from "./CategoryIcon";
import { getCategoryBySlug, type CategoryDetail } from "@/lib/api";

/**
 * A store's category landing (Wave 56):
 * /<store>/categories/<segment> — the CategoryLanding
 * idea scoped to ONE store. The segment resolves the
 * same way (taxonomy slug → subtree browse pinned to
 * the store; anything else → the Wave 53 in-store text
 * landing), so a store's specialisation tiles can link
 * here and the shopper stays inside the store instead
 * of being carried to the marketplace-wide category.
 * Children tiles stay in-store too. The store's own
 * header (StoreShopHeader) is rendered by the page.
 */
export default function StoreCategoryLanding({
  storeSlug,
  segment,
}: {
  storeSlug: string;
  segment: string;
}) {
  const [detail, setDetail] = useState<CategoryDetail | null>(null);
  const [resolved, setResolved] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getCategoryBySlug(segment)
      .then((found) => {
        if (!cancelled) {
          setDetail(found);
          setResolved(true);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setDetail(null);
          setResolved(true);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [segment]);

  if (!resolved) {
    return <p>লোড হচ্ছে… (Loading…)</p>;
  }

  const basePath = `/${storeSlug}/categories/${encodeURIComponent(segment)}`;

  if (detail === null) {
    // Legacy in-store label landing.
    return (
      <BrowseResults
        basePath={basePath}
        heading={`ক্যাটাগরি: ${segment}`}
        forcedStoreSlug={storeSlug}
        forcedCategory={segment}
      />
    );
  }

  const { category, path } = detail;
  return (
    <>
      <nav aria-label="breadcrumb" className="category-breadcrumb">
        <Link href={`/${storeSlug}`}>স্টোর — Store</Link>
        {path.map((entry) => (
          <span key={entry.id}>
            {" › "}
            {entry.id === category.id ? (
              <span aria-current="page">{entry.name_bn ?? entry.name_en}</span>
            ) : (
              <Link href={`/${storeSlug}/categories/${entry.slug}`}>
                {entry.name_bn ?? entry.name_en}
              </Link>
            )}
          </span>
        ))}
      </nav>
      <h1>
        <CategoryIcon iconKey={category.icon_key} />{" "}
        {category.name_bn ?? category.name_en}
        {category.name_bn && (
          <span className="category-landing-en"> — {category.name_en}</span>
        )}
      </h1>
      {category.children.length > 0 && (
        <div className="category-index-children">
          {category.children.map((child) => (
            <Link
              key={child.id}
              href={`/${storeSlug}/categories/${child.slug}`}
              className="category-chip"
            >
              <CategoryIcon iconKey={child.icon_key} />{" "}
              {child.name_bn ?? child.name_en}
            </Link>
          ))}
        </div>
      )}
      <BrowseResults
        basePath={basePath}
        heading={`${category.name_bn ?? category.name_en} — এই স্টোরে`}
        forcedStoreSlug={storeSlug}
        forcedCategorySlug={category.slug}
      />
    </>
  );
}
