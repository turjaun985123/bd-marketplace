"use client";

import { useEffect, useRef, useState, type ReactNode } from "react";
import ProductGrid from "./ProductGrid";
import StoreCategories from "./StoreCategories";
import StoreCollections from "./StoreCollections";
import StoreSpecialties from "./StoreSpecialties";
import StorefrontSections from "./StorefrontSections";
import StorefrontSectionView, {
  type SectionViewContext,
} from "./StorefrontSectionViews";
import {
  getStoreLayout,
  recordSectionEvent,
  type PublicStore,
  type PublicStoreLayout,
} from "@/lib/api";

/**
 * The storefront's section composition (Wave 58): the
 * page body below the store's identity frame is no
 * longer fixed code — it is the store's LAYOUT, resolved
 * server-side by GET /v1/stores/:slug/layout and rendered
 * here section by section through the shared section
 * components.
 *
 * Placement rule: HERO sections render ABOVE the
 * identity frame (the header / stats / follow / search
 * block this component wraps as `children`) — the hero
 * carries the announcement bar and the banner, which
 * crowned the page long before sections existed — and
 * every other section renders below the frame, in layout
 * order. With the platform default layout that reproduces
 * the pre-Wave-58 page exactly.
 *
 * Analytics: each rendered section fires one
 * `section_view` beacon when the layout loads, and hero
 * / banner link clicks fire `section_click` — both
 * aggregate-only on the API (no identity is sent, ever).
 * Beacons are fire-and-forget: a failed beacon never
 * disturbs the page.
 *
 * Failure rule: if the layout read fails, the page falls
 * back to the LEGACY fixed composition (the Wave 52–56
 * components in their historical order) rather than
 * showing a broken storefront — a storefront must never
 * blank over a sections outage.
 */
export default function StorefrontLayout({
  slug,
  store,
  children,
}: {
  slug: string;
  store: PublicStore;
  children: ReactNode;
}) {
  const [layout, setLayout] = useState<PublicStoreLayout | null>(null);
  const [failed, setFailed] = useState(false);
  const viewedRef = useRef<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getStoreLayout(slug)
      .then((data) => {
        if (!cancelled) setLayout(data);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [slug]);

  // One view beacon per rendered section per page load.
  useEffect(() => {
    if (!layout || viewedRef.current === layout.store_slug) return;
    viewedRef.current = layout.store_slug;
    for (const section of layout.sections) {
      recordSectionEvent(slug, section.id, "section_view").catch(() => {
        /* analytics never disturbs the page */
      });
    }
  }, [layout, slug]);

  const theme = store.theme_key;
  const context: SectionViewContext = {
    slug,
    theme,
    accent: store.brand_color ?? undefined,
    gridLayout:
      theme === "minimal" ? "list" : theme === "bold" ? "large" : "grid",
    onSectionClick: (sectionId) => {
      recordSectionEvent(slug, sectionId, "section_click").catch(() => {
        /* analytics never disturbs the page */
      });
    },
  };

  if (failed) {
    return (
      <>
        <LegacyHero store={store} />
        {children}
        <LegacyStorefrontSections slug={slug} store={store} />
      </>
    );
  }
  if (layout === null) {
    // The frame renders immediately; sections pop in when
    // the layout lands (the pre-Wave-58 components all
    // loaded their own data asynchronously too).
    return <>{children}</>;
  }

  const heroes = layout.sections.filter((section) => section.type === "hero");
  const rest = layout.sections.filter((section) => section.type !== "hero");
  return (
    <>
      {heroes.map((section) => (
        <StorefrontSectionView
          key={section.id}
          section={section}
          context={context}
        />
      ))}
      {children}
      {rest.map((section) => (
        <StorefrontSectionView
          key={section.id}
          section={section}
          context={context}
        />
      ))}
    </>
  );
}

/**
 * The pre-Wave-58 hero pieces (announcement bar + banner
 * image under the theme's banner rule), kept for the
 * failure fallback: they are the hero SECTION's payload
 * in the layout world, and the frame's crown in the
 * legacy one.
 */
function LegacyHero({ store }: { store: PublicStore }) {
  const theme = store.theme_key;
  const accent = store.brand_color ?? undefined;
  return (
    <>
      {store.announcement && (
        <p
          className="announcement-bar"
          style={accent ? { background: accent, color: "#fff" } : undefined}
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
    </>
  );
}

/**
 * The pre-Wave-58 fixed composition, kept as the failure
 * fallback: the Wave 52 rails, the Wave 54 collections,
 * the Wave 56 specialties, the Wave 54 category tiles,
 * the catalogue grid and the profile text blocks, in
 * their historical order.
 */
function LegacyStorefrontSections({
  slug,
  store,
}: {
  slug: string;
  store: PublicStore;
}) {
  const theme = store.theme_key;
  return (
    <>
      <StorefrontSections slug={slug} />
      <StoreCollections slug={slug} />
      <StoreSpecialties slug={slug} />
      <StoreCategories slug={slug} />
      <h2>সব পণ্য — All products</h2>
      <ProductGrid
        storeSlug={slug}
        layout={
          theme === "minimal" ? "list" : theme === "bold" ? "large" : "grid"
        }
      />
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
    </>
  );
}
