"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import { EmptyState, Skeleton } from "@/components/ui";
import { getHomeFeed, type HomeCategory } from "@/lib/api";

/**
 * The category index (Wave 52): the platform's categories
 * as a browsable tile page, from the same home-feed
 * category aggregate the home tiles use (product counts
 * over active products in active stores, each tile's cover
 * from the category's best-selling product that has an
 * image; the feed's named category cap applies here too).
 *
 * Categories are still the products' free-text labels —
 * there is no categories table — so this page is a view of
 * the catalogue's own vocabulary, and each tile lands on
 * the search page filtered to that label.
 */
export default function CategoriesPage() {
  const [categories, setCategories] = useState<HomeCategory[] | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getHomeFeed()
      .then((feed) => {
        if (!cancelled) setCategories(feed.categories);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <main>
      <p>
        <Link href="/">← মার্কেটপ্লেস</Link>
      </p>
      <h1>সব ক্যাটাগরি — All categories</h1>
      {failed ? (
        <EmptyState title="ক্যাটাগরি লোড করা যায়নি — Categories could not load">
          <p>একটু পরে আবার চেষ্টা করুন। (Please try again shortly.)</p>
        </EmptyState>
      ) : categories === null ? (
        <div className="category-tiles" aria-hidden="true">
          {Array.from({ length: 6 }, (_, i) => (
            <Skeleton key={i} height={72} />
          ))}
        </div>
      ) : categories.length === 0 ? (
        <EmptyState title="এখনো কোনো ক্যাটাগরি নেই — No categories yet">
          <p>
            কোনো পণ্যে ক্যাটাগরি দেওয়া হলেই এখানে দেখা যাবে।
            (Categories appear as soon as products carry them.)
          </p>
        </EmptyState>
      ) : (
        <div className="category-tiles">
          {categories.map((category) => (
            <Link
              key={category.category}
              href={`/search?category=${encodeURIComponent(category.category)}`}
              className="category-tile"
            >
              {category.image_url ? (
                // eslint-disable-next-line @next/next/no-img-element
                <img
                  src={category.image_url}
                  alt={category.category}
                  className="category-tile-image"
                  loading="lazy"
                />
              ) : (
                <span
                  className="category-tile-image product-image-placeholder"
                  aria-hidden="true"
                >
                  {category.category.slice(0, 1)}
                </span>
              )}
              <span className="category-tile-body">
                <span className="category-tile-name">{category.category}</span>
                <span className="category-tile-count">
                  {category.product_count}টি পণ্য — products
                </span>
              </span>
            </Link>
          ))}
        </div>
      )}
    </main>
  );
}
