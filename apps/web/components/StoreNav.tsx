"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { listStorePages, type PublicPageSummary } from "@/lib/api";

/**
 * The storefront navigation (Wave 59): Home, Shop, the
 * store's published nav pages in the seller's order,
 * and Blog — rendered from the ONE public pages read,
 * which also carries the live blog-post count, so the
 * Blog link appears only when the blog actually has a
 * live post (no dead links to an empty blog). The nav
 * renders on the storefront home, the shop header and
 * every content page, so a visitor can always move
 * around the store. A failed read renders just Home and
 * Shop — the two links that need no data.
 */
export default function StoreNav({
  slug,
  current,
}: {
  slug: string;
  /** Which surface is showing: "home" | "shop" | "blog" | a page slug. */
  current?: string;
}) {
  const [pages, setPages] = useState<PublicPageSummary[]>([]);
  const [blogCount, setBlogCount] = useState(0);

  useEffect(() => {
    let cancelled = false;
    listStorePages(slug)
      .then((data) => {
        if (!cancelled) {
          setPages(data.items);
          setBlogCount(data.blog_post_count);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setPages([]);
          setBlogCount(0);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [slug]);

  return (
    <nav className="store-nav" aria-label="Store">
      <Link href={`/${slug}`} aria-current={current === "home" ? "page" : undefined}>
        হোম — Home
      </Link>
      <Link
        href={`/${slug}/shop`}
        aria-current={current === "shop" ? "page" : undefined}
      >
        সব পণ্য — Shop
      </Link>
      {pages.map((page) => (
        <Link
          key={page.slug}
          href={`/${slug}/pages/${page.slug}`}
          aria-current={current === page.slug ? "page" : undefined}
        >
          {page.title}
        </Link>
      ))}
      {blogCount > 0 && (
        <Link
          href={`/${slug}/blog`}
          aria-current={current === "blog" ? "page" : undefined}
        >
          ব্লগ — Blog
        </Link>
      )}
    </nav>
  );
}
