"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import BrowseResults from "./BrowseResults";
import CategoryIcon from "./CategoryIcon";
import { getCategoryBySlug, type CategoryDetail } from "@/lib/api";

/**
 * The category landing resolver (Wave 56): the
 * /categories/<segment> route serves TWO kinds of
 * landings from one segment —
 *
 * - a TAXONOMY node (the segment is a category slug):
 *   the node's own page — breadcrumb path, icon, Bangla +
 *   English names, children tiles, and the browse grid
 *   scoped to the node's whole SUBTREE via the API's
 *   `category_slug` filter;
 * - a legacy free-text LABEL (anything else): the Wave 53
 *   landing, the browse pinned to that label.
 *
 * The resolver asks the API which one the segment is
 * (GET /v1/categories/:slug answers 404 for labels); while
 * the answer is in flight it renders nothing but a loading
 * line, so a label landing never flashes a taxonomy frame
 * or vice versa.
 */
export default function CategoryLanding({ segment }: { segment: string }) {
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

  if (detail === null) {
    // Legacy label landing (Wave 53 behaviour, unchanged).
    return (
      <BrowseResults
        basePath={`/categories/${encodeURIComponent(segment)}`}
        heading={`ক্যাটাগরি: ${segment}`}
        forcedCategory={segment}
      />
    );
  }

  const { category, path } = detail;
  return (
    <>
      <nav aria-label="breadcrumb" className="category-breadcrumb">
        <Link href="/">হোম — Home</Link>
        {" › "}
        <Link href="/categories">ক্যাটাগরি — Categories</Link>
        {path.map((entry) => (
          <span key={entry.id}>
            {" › "}
            {entry.id === category.id ? (
              <span aria-current="page">{entry.name_bn ?? entry.name_en}</span>
            ) : (
              <Link href={`/categories/${entry.slug}`}>
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
      <p>
        এই ক্যাটাগরি ও এর সব উপ-ক্যাটাগরিতে মোট{" "}
        {category.product_count}টি পণ্য — {category.product_count}{" "}
        products in this category and its sub-categories.
      </p>
      {category.children.length > 0 && (
        <div className="category-tiles">
          {category.children.map((child) => (
            <Link
              key={child.id}
              href={`/categories/${child.slug}`}
              className="category-tile"
            >
              {child.image_url ? (
                // eslint-disable-next-line @next/next/no-img-element
                <img
                  src={child.image_url}
                  alt={child.name_en}
                  className="category-tile-image"
                  loading="lazy"
                />
              ) : (
                <span
                  className="category-tile-image product-image-placeholder"
                  aria-hidden="true"
                >
                  <CategoryIcon iconKey={child.icon_key} />
                </span>
              )}
              <span className="category-tile-body">
                <span className="category-tile-name">
                  {child.name_bn ?? child.name_en}
                </span>
                <span className="category-tile-count">
                  {child.product_count}টি পণ্য — products
                </span>
              </span>
            </Link>
          ))}
        </div>
      )}
      <BrowseResults
        basePath={`/categories/${encodeURIComponent(segment)}`}
        heading={`${category.name_bn ?? category.name_en} — পণ্যসমূহ`}
        forcedCategorySlug={category.slug}
      />
    </>
  );
}
