"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import CategoryIcon from "@/components/CategoryIcon";
import { EmptyState, Skeleton } from "@/components/ui";
import {
  getCategories,
  type CategoryTreeNode,
  type HomeCategory,
} from "@/lib/api";

/**
 * The category index (Wave 52/53; taxonomy in Wave 56):
 * one fetch of GET /v1/categories now carries BOTH
 * vocabularies the platform has —
 *
 * - the managed TAXONOMY tree (the response's `tree`
 *   field): curated categories with Bangla + English
 *   names, icons and subtree product counts, rendered as
 *   sections — each top-level node with its children as
 *   chips — landing on the node's own page
 *   (/categories/<slug>, subtree browse);
 * - the legacy free-text LABELS (the `categories` field,
 *   byte-identical to the Wave 53 aggregate), rendered
 *   below as "more labels" for the labels no taxonomy node
 *   covers — free-text products still exist by design,
 *   and their labels keep a browsable home here. A label
 *   a taxonomy node already covers (same slug or English
 *   name) is not repeated.
 *
 * The find box filters both vocabularies.
 */
export default function CategoriesPage() {
  const [categories, setCategories] = useState<HomeCategory[] | null>(null);
  const [tree, setTree] = useState<CategoryTreeNode[] | null>(null);
  const [failed, setFailed] = useState(false);
  // Wave 53: a find-as-you-type box over the lists — on a
  // mature catalogue the vocabulary outgrows one screen.
  const [filterText, setFilterText] = useState("");

  useEffect(() => {
    let cancelled = false;
    getCategories()
      .then((list) => {
        if (!cancelled) {
          setCategories(list.categories);
          setTree(list.tree ?? []);
        }
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const needle = filterText.trim().toLowerCase();
  const matches = (text: string) =>
    needle === "" || text.toLowerCase().includes(needle);

  // Labels the taxonomy already covers (by slug or
  // English name) are not repeated in the labels section.
  const covered = new Set<string>();
  const collectCovered = (nodes: CategoryTreeNode[]) => {
    for (const node of nodes) {
      covered.add(node.slug.toLowerCase());
      covered.add(node.name_en.toLowerCase());
      collectCovered(node.children);
    }
  };
  if (tree) collectCovered(tree);

  const visibleRoots = (tree ?? []).filter(
    (node) =>
      matches(node.name_en) ||
      matches(node.name_bn ?? "") ||
      matches(node.slug) ||
      node.children.some(
        (child) => matches(child.name_en) || matches(child.name_bn ?? "")
      )
  );
  const visibleLabels = (categories ?? []).filter(
    (category) =>
      !covered.has(category.category.toLowerCase()) &&
      matches(category.category)
  );
  const loaded = categories !== null && tree !== null;
  const nothingFound =
    loaded && visibleRoots.length === 0 && visibleLabels.length === 0;

  return (
    <main>
      <p>
        <Link href="/">← মার্কেটপ্লেস</Link>
      </p>
      <h1>সব ক্যাটাগরি — All categories</h1>
      {loaded && (visibleRoots.length > 6 || visibleLabels.length > 6) && (
        <p>
          <label>
            ক্যাটাগরি খুঁজুন — Find a category{" "}
            <input
              type="search"
              value={filterText}
              onChange={(e) => setFilterText(e.target.value)}
              placeholder="যেমন: clothing"
            />
          </label>
        </p>
      )}
      {failed ? (
        <EmptyState title="ক্যাটাগরি লোড করা যায়নি — Categories could not load">
          <p>একটু পরে আবার চেষ্টা করুন। (Please try again shortly.)</p>
        </EmptyState>
      ) : !loaded ? (
        <div className="category-tiles" aria-hidden="true">
          {Array.from({ length: 6 }, (_, i) => (
            <Skeleton key={i} height={72} />
          ))}
        </div>
      ) : nothingFound ? (
        <EmptyState title="কোনো ক্যাটাগরি মেলেনি — No categories match">
          <p>
            অন্য নামে খুঁজে দেখুন। (Try a different name.)
          </p>
        </EmptyState>
      ) : (
        <>
          {visibleRoots.map((node) => (
            <section key={node.id} className="category-index-section">
              <div className="category-tiles">
                <Link
                  href={`/categories/${node.slug}`}
                  className="category-tile"
                >
                  {node.image_url ? (
                    // eslint-disable-next-line @next/next/no-img-element
                    <img
                      src={node.image_url}
                      alt={node.name_en}
                      className="category-tile-image"
                      loading="lazy"
                    />
                  ) : (
                    <span
                      className="category-tile-image product-image-placeholder"
                      aria-hidden="true"
                    >
                      <CategoryIcon iconKey={node.icon_key} />
                    </span>
                  )}
                  <span className="category-tile-body">
                    <span className="category-tile-name">
                      {node.name_bn ?? node.name_en}
                    </span>
                    <span className="category-tile-count">
                      {node.product_count}টি পণ্য — products
                    </span>
                  </span>
                </Link>
              </div>
              {node.children.length > 0 && (
                <p className="category-index-children">
                  {node.children
                    .filter(
                      (child) =>
                        matches(child.name_en) || matches(child.name_bn ?? "")
                    )
                    .map((child) => (
                      <Link
                        key={child.id}
                        href={`/categories/${child.slug}`}
                        className="category-chip"
                      >
                        <CategoryIcon iconKey={child.icon_key} />{" "}
                        {child.name_bn ?? child.name_en} (
                        {child.product_count})
                      </Link>
                    ))}
                </p>
              )}
            </section>
          ))}

          {visibleLabels.length > 0 && (
            <>
              <h2>আরও লেবেল — More labels</h2>
              <p>
                বিক্রেতাদের দেওয়া মুক্ত লেবেল, যেগুলো এখনো
                ক্যাটাগরি তালিকায় নেই — Free-text labels
                sellers use that are not taxonomy nodes yet.
              </p>
              <div className="category-tiles">
                {visibleLabels.map((category) => (
                  <Link
                    key={category.category}
                    href={`/browse?category=${encodeURIComponent(category.category)}`}
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
                      <span className="category-tile-name">
                        {category.category}
                      </span>
                      <span className="category-tile-count">
                        {category.product_count}টি পণ্য — products
                      </span>
                    </span>
                  </Link>
                ))}
              </div>
            </>
          )}
        </>
      )}
    </main>
  );
}
