"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import HeroCarousel from "./HeroCarousel";
import ProductCard from "./ProductCard";
import ProductRail from "./ProductRail";
import { Countdown, ProductRailSkeleton, Rail, SectionHeader } from "./ui";
import {
  getHomeFeed,
  type HomeFeed as HomeFeedData,
  type HomeFlashItem,
} from "@/lib/api";

/**
 * The merchandised home page body (Wave 52): one fetch of
 * GET /v1/home rendered as the Daraz/AliExpress sequence —
 * hero campaigns, category tiles, the flash sale with its
 * countdown, curated collection sections, the discovery
 * rails and featured verified stores. Every section hides
 * itself when the feed has nothing honest for it (the API
 * already omits empty content; the checks here are the
 * rendering of that contract), and the whole body shows
 * skeletons while the feed loads so the page never jumps.
 *
 * The personalised / recently-viewed strips stay in
 * HomeDiscovery (they are per-visitor, not feed content);
 * the plain catalogue grid stays below this component.
 */
export default function HomeFeed() {
  const [feed, setFeed] = useState<HomeFeedData | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getHomeFeed()
      .then((data) => {
        if (!cancelled) setFeed(data);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (failed) {
    // The catalogue grid below is the page's fallback — a
    // failed feed must not blank the home page.
    return null;
  }
  if (feed === null) {
    return (
      <>
        <div className="skeleton" style={{ height: 220, borderRadius: 14 }} />
        <ProductRailSkeleton count={4} />
      </>
    );
  }

  return (
    <>
      <HeroCarousel banners={feed.banners} />

      {feed.categories.length > 0 && (
        <section>
          <SectionHeader
            title="ক্যাটাগরি — Categories"
            linkHref="/categories"
            linkLabel="সব ক্যাটাগরি — All categories"
          />
          <div className="category-tiles">
            {feed.categories.map((category) => (
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
        </section>
      )}

      {feed.flash_sale && feed.flash_sale.items.length > 0 && (
        <section className="flash-section">
          <div className="section-header">
            <h2 className="flash-title">⚡ {feed.flash_sale.title}</h2>
            <Countdown endsAt={feed.flash_sale.ends_at} />
          </div>
          <Rail cards>
            {feed.flash_sale.items.map((item) => (
              <div key={item.id}>
                <FlashCard product={item} />
                {item.stock_cap !== null && (
                  <>
                    <div
                      className="flash-progress"
                      role="progressbar"
                      aria-valuenow={item.sold_at_sale}
                      aria-valuemax={item.stock_cap}
                    >
                      <div
                        className="flash-progress-bar"
                        style={{
                          width: `${Math.min(
                            100,
                            Math.round((item.sold_at_sale * 100) / item.stock_cap)
                          )}%`,
                        }}
                      />
                    </div>
                    <p className="flash-progress-text">
                      {item.sold_at_sale}/{item.stock_cap} বিক্রি হয়েছে — sold
                    </p>
                  </>
                )}
              </div>
            ))}
          </Rail>
        </section>
      )}

      {feed.collections.map((collection) => (
        <ProductRail
          key={collection.id}
          title={collection.title}
          products={collection.products}
          linkHref={`/collections/${collection.slug}`}
          linkLabel="কালেকশন দেখুন — View collection"
        />
      ))}

      <ProductRail title="নতুন এসেছে — New arrivals" products={feed.new_arrivals} />
      <ProductRail title="সেরা বিক্রিত — Best sellers" products={feed.best_sellers} />
      <ProductRail title="টপ রেটেড — Top rated" products={feed.top_rated} />
      <ProductRail title="স্পন্সরড — Sponsored" products={feed.promoted} />

      {feed.featured_stores.length > 0 && (
        <section>
          <SectionHeader title="যাচাইকৃত স্টোর — Featured stores" />
          <div className="featured-stores">
            {feed.featured_stores.map((store) => (
              <Link
                key={store.id}
                href={`/${store.slug}`}
                className="featured-store"
              >
                {store.banner_url ? (
                  // eslint-disable-next-line @next/next/no-img-element
                  <img
                    src={store.banner_url}
                    alt=""
                    className="featured-store-banner"
                    loading="lazy"
                  />
                ) : (
                  <span className="featured-store-banner" aria-hidden="true" />
                )}
                <span className="featured-store-body">
                  {store.logo_url ? (
                    // eslint-disable-next-line @next/next/no-img-element
                    <img
                      src={store.logo_url}
                      alt=""
                      className="featured-store-logo"
                      loading="lazy"
                    />
                  ) : null}
                  <span>
                    <span className="featured-store-name">{store.name}</span>
                    <span className="featured-store-count">
                      {store.product_count}টি পণ্য — products
                    </span>
                  </span>
                </span>
              </Link>
            ))}
          </div>
        </section>
      )}
    </>
  );
}

/** One flash-sale card: the shared card priced at the sale terms. */
function FlashCard({ product }: { product: HomeFlashItem }) {
  const item = product;
  return (
    <ProductCard
      productId={item.id}
      name={item.title_bn ?? item.title}
      storeName={item.store_slug}
      pricePoisha={item.price_poisha}
      href={`/products/${item.id}`}
      ratingAvg={item.rating_avg}
      ratingCount={item.rating_count}
      isPromoted={item.is_promoted}
      imageUrl={item.image_url}
      hoverImageUrl={item.hover_image_url}
      compareAtPoisha={item.compare_at_poisha}
      soldCount={item.sold_count}
      flashPricePoisha={item.flash_price_poisha}
      flashEndsAt={item.flash_ends_at}
    />
  );
}
