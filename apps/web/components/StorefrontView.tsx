"use client";

import { useEffect, useState, type CSSProperties } from "react";
import Link from "next/link";
import FollowButton from "./FollowButton";
import MessageStoreButton from "./MessageStoreButton";
import ProductGrid from "./ProductGrid";
import StoreCategories from "./StoreCategories";
import StoreSpecialties from "./StoreSpecialties";
import StoreCollections from "./StoreCollections";
import StoreStats from "./StoreStats";
import StorefrontSections from "./StorefrontSections";
import SearchBar from "./SearchBar";
import VerificationBadge from "./VerificationBadge";
import { useAuth } from "@/hooks/useAuth";
import { getStore, type PublicStore } from "@/lib/api";

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
  // Wave 54: the public store view — follower count, the
  // viewer's follow state and the trust aggregates ride
  // the same read; the token makes the follow state the
  // viewer's own.
  const { token } = useAuth();
  const [store, setStore] = useState<PublicStore | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getStore(slug, token)
      .then((data) => {
        if (!cancelled) setStore(data);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [slug, token]);

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
        <VerificationBadge tier={store.verified_tier} />
      </div>
      {store.description && <p>{store.description}</p>}
      <p>
        এটি <strong>{store.name}</strong>-এর নিজস্ব স্টোরফ্রন্ট — একই পণ্য
        কেন্দ্রীয় মার্কেটপ্লেসেও পাওয়া যায়।
      </p>

      {/* Wave 54: the trust line (rating, sold, followers,
          joined) and the follow action. */}
      <StoreStats store={store} />
      <p>
        <FollowButton
          slug={store.slug}
          storeOwnerId={store.owner_user_id}
          initialFollowing={store.is_following}
          initialCount={store.follower_count}
        />
      </p>

      <MessageStoreButton slug={slug} />
      <SearchBar storeSlug={slug} />
      <p>
        <Link href={`/${slug}/shop`}>
          ফিল্টারসহ সব পণ্য দেখুন — Shop all products with filters
        </Link>
      </p>
      {/* Wave 52: the store's merchandising sections (on
          sale / best sellers / new), above the full grid. */}
      <StorefrontSections slug={slug} />
      {/* Wave 54 extension: the seller's own collections. */}
      <StoreCollections slug={slug} />
      {/* Wave 56: the store's taxonomy specialisation. */}
      <StoreSpecialties slug={slug} />
      {/* Wave 54: the store's own category tiles. */}
      <StoreCategories slug={slug} />
      <h2>সব পণ্য — All products</h2>
      <ProductGrid
        storeSlug={slug}
        layout={
          theme === "minimal" ? "list" : theme === "bold" ? "large" : "grid"
        }
      />
      {/* Wave 54: the seller's own long-form texts, when
          they have written them (seller settings). */}
      {store.about && (
        <section className="store-text-block">
          <h2>স্টোর সম্পর্কে — About this store</h2>
          <p className="store-long-text">{store.about}</p>
        </section>
      )}
      {store.return_policy && (
        <section className="store-text-block">
          <h2>রিটার্ন নীতি — Return policy</h2>
          <p className="store-long-text">{store.return_policy}</p>
        </section>
      )}
    </div>
  );
}
