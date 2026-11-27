"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { getCategoryTree, type CategoryTreeNode } from "@/lib/api";
import CategoryIcon from "@/components/CategoryIcon";

/**
 * The marketplace category navigation bar (Wave 56):
 * rides in the marketplace frame under the flash strip,
 * on every marketplace page. The quick row shows the
 * top-level taxonomy nodes (featured first, then display
 * order — the home strip's order); the "সব ক্যাটাগরি"
 * button opens the mega panel with the whole tree —
 * every root as a column, its children as links, its
 * grandchildren nested under their parents.
 *
 * Data is the public tree endpoint (GET
 * /v1/categories/tree): one fetch per mount, no per-page
 * wiring. The bar renders NOTHING until the tree arrives
 * and nothing at all if it fails or is empty — a frame
 * element must never break or block the page under it.
 * Panel behaviour: closes on link click, on Escape, and
 * on a click outside the bar; the toggle is a real
 * button with aria-expanded, the panel a labelled nav.
 */

/** How many roots ride the always-visible quick row. */
const QUICK_ROOT_LIMIT = 7;

export default function CategoryNav() {
  const [tree, setTree] = useState<CategoryTreeNode[] | null>(null);
  const [open, setOpen] = useState(false);
  const barRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    let cancelled = false;
    getCategoryTree()
      .then((body) => {
        if (!cancelled) setTree(body.tree);
      })
      .catch(() => {
        if (!cancelled) setTree([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Escape closes; a pointer-down outside the bar closes.
  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    const onPointer = (event: MouseEvent) => {
      if (barRef.current && !barRef.current.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onPointer);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onPointer);
    };
  }, [open ]);

  if (tree === null || tree.length === 0) return null;

  const quickRoots = [...tree]
    .sort((a, b) => Number(b.is_featured) - Number(a.is_featured))
    .slice(0, QUICK_ROOT_LIMIT);

  return (
    <div className="category-nav" ref={barRef}>
      <div className="category-nav-bar">
        <button
          type="button"
          className="category-nav-toggle"
          aria-expanded={open}
          aria-controls="category-nav-panel"
          onClick={() => setOpen((value) => !value)}
        >
          ☰ সব ক্যাটাগরি — All categories
        </button>
        <nav className="category-nav-quick" aria-label="ক্যাটাগরি">
          {quickRoots.map((root) => (
            <Link
              key={root.id}
              href={`/categories/${root.slug}`}
              className="category-nav-quick-link"
            >
              <CategoryIcon iconKey={root.icon_key} />{" "}
              {root.name_bn ?? root.name_en}
            </Link>
          ))}
          <Link href="/categories" className="category-nav-quick-link">
            সব দেখুন — View all
          </Link>
        </nav>
      </div>
      {open ? (
        <nav
          id="category-nav-panel"
          className="category-nav-panel"
          aria-label="সব ক্যাটাগরি — All categories"
        >
          {tree.map((root) => (
            <div key={root.id} className="category-nav-col">
              <Link
                href={`/categories/${root.slug}`}
                className="category-nav-root"
                onClick={() => setOpen(false)}
              >
                <CategoryIcon iconKey={root.icon_key} />{" "}
                {root.name_bn ?? root.name_en}
                <span className="category-nav-count">
                  {root.product_count.toLocaleString("en-IN")}
                </span>
              </Link>
              <span className="category-nav-root-en">{root.name_en}</span>
              {root.children.map((child) => (
                <span key={child.id} className="category-nav-child-wrap">
                  <Link
                    href={`/categories/${child.slug}`}
                    className="category-nav-child"
                    onClick={() => setOpen(false)}
                  >
                    {child.name_bn ?? child.name_en}
                  </Link>
                  {child.children.map((grandchild) => (
                    <Link
                      key={grandchild.id}
                      href={`/categories/${grandchild.slug}`}
                      className="category-nav-grandchild"
                      onClick={() => setOpen(false)}
                    >
                      {grandchild.name_bn ?? grandchild.name_en}
                    </Link>
                  ))}
                </span>
              ))}
            </div>
          ))}
        </nav>
      ) : null}
    </div>
  );
}
