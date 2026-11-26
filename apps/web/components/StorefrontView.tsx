"use client";

import { useEffect, useState, type CSSProperties } from "react";
import MessageStoreButton from "./MessageStoreButton";
import ProductGrid from "./ProductGrid";
import SearchBar from "./SearchBar";
import { getStore, type Store } from "@/lib/api";

/**
 * The ONE storefront view (Wave 18). The store's `theme_key`
 * selects a layout variant of this single component — never a
 * separate page per theme:
 *
 * - classic: banner image (when set) over a logo + name header,
 *   the standard product grid — the pre-Wave-18 look.
 * - minimal: no banner, a small muted header, products as
 *   compact list rows.
 * - bold: a large banner, an oversized store name, products as
 *   oversized cards in a two-column grid.
 *
 * The brand colour is an ACCENT only: it paints the announcement
 * bar and, through the `--store-accent` CSS variable scoped to
 * this view's root, the button borders inside the storefront.
 * Sellers supply validated values (a theme key, one hex colour,
 * image URLs, short texts) — never markup or CSS.
 */
export default function StorefrontView({ slug }: { slug: string }) {
  const [store, setStore] = useState<Store | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getStore(slug)
      .then((data) => {
        if (!cancelled) setStore(data);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [slug]);

  if (failed) {
    return (
      <p>
        এই স্টোরটি পাওয়া যায়নি। (Store not found — check the address, or
        the store may have been removed.)
      </p>
    );
  }
  if (store === null) {
    return <p>স্টোর লোড হচ্ছে… (Loading store…)</p>;
  }

  const theme = store.theme_key;
  const accent = store.brand_color ?? undefined;

  return (
    <div
      className={`storefront storefront--${theme}`}
      style={
        accent ? ({ "--store-accent": accent } as CSSProperties) : undefined
      }
    >
      {store.announcement && (
        <p
          className="announcement-bar"
          style={
            accent ? { background: accent, color: "#fff" } : undefined
          }
        >
          {store.announcement}
        </p>
      )}

      {theme !== "minimal" && store.banner_url && (
        // eslint-disable-next-line @next/next/no-img-element
        <img
          className={
            theme === "bold"
              ? "storefront-banner storefront-banner--large"
              : "storefront-banner"
          }
          src={store.banner_url}
          alt=""
        />
      )}

      <div className="storefront-header">
        {store.logo_url && (
          // eslint-disable-next-line @next/next/no-img-element
          <img
            className={
              theme === "bold"
                ? "storefront-logo storefront-logo--large"
                : theme === "minimal"
                  ? "storefront-logo storefront-logo--small"
                  : "storefront-logo"
            }
            src={store.logo_url}
            alt=""
          />
        )}
        <h1>{store.name}</h1>
      </div>
      {store.description && <p>{store.description}</p>}
      <p>
        এটি <strong>{store.name}</strong>-এর নিজস্ব স্টোরফ্রন্ট — একই পণ্য
        কেন্দ্রীয় মার্কেটপ্লেসেও পাওয়া যায়।
      </p>

      <MessageStoreButton slug={slug} />
      <SearchBar storeSlug={slug} />
      <h2>পণ্য — Products</h2>
      <ProductGrid
        storeSlug={slug}
        layout={
          theme === "minimal" ? "list" : theme === "bold" ? "large" : "grid"
        }
      />
    </div>
  );
}
