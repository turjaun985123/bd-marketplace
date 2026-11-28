"use client";

import Link from "next/link";
import CategoryIcon from "./CategoryIcon";
import ProductGrid from "./ProductGrid";
import ProductImage from "./ProductImage";
import ProductRail from "./ProductRail";
import type {
  ResolvedSection,
  StoreTheme,
} from "@/lib/api";

/**
 * The storefront section renderers (Wave 58): one React
 * component per section type, rendering a RESOLVED
 * section (the API's public layout model — every payload
 * already loaded server-side) into the storefront's
 * existing visual vocabulary: the rails are the shared
 * ProductRail, the shelves and grids reuse the Wave
 * 54/56 tile markup and classes, the info blocks reuse
 * the store-text-block frame. The seller editor's live
 * preview renders through these SAME components with
 * client-resolved sections, so what the seller previews
 * is the markup the storefront serves.
 *
 * The theme still frames everything: the hero's banner
 * follows the theme's banner rule (minimal hides it,
 * bold enlarges it), the announcement bar takes the
 * brand accent, and the catalogue grid takes the theme's
 * card layout. Sections are layout; theme is skin.
 */

export interface SectionViewContext {
  /** The store's slug (deep links are store-relative). */
  slug: string;
  theme: StoreTheme;
  /** The brand accent, when the store set one. */
  accent?: string;
  /** The catalogue grid's card layout (the theme's). */
  gridLayout: "grid" | "list" | "large";
  /** Section link clicks (analytics); absent in preview. */
  onSectionClick?: (sectionId: string) => void;
}

/** A string out of a section config, or null. */
function cfgStr(config: Record<string, unknown>, key: string): string | null {
  const value = config[key];
  return typeof value === "string" && value.length > 0 ? value : null;
}

/** The rail title defaults — the pre-Wave-58 storefront's own words. */
const RAIL_DEFAULT_TITLES: Record<string, string> = {
  on_sale: "অফারে আছে — On sale at this store",
  best_sellers: "এই স্টোরের সেরা বিক্রিত — This store's best sellers",
  newest: "নতুন এসেছে — New at this store",
  manual: "বাছাই করা পণ্য — Hand-picked",
};

/** Where a rail's "see all" link lands, per source. */
function railLink(
  slug: string,
  source: string
): { href: string; label: string } | null {
  switch (source) {
    case "on_sale":
      return { href: `/${slug}/shop?on_sale=true`, label: "সব অফার — All offers" };
    case "best_sellers":
      return { href: `/${slug}/shop?sort=sold`, label: "সব দেখুন — See all" };
    case "newest":
      return { href: `/${slug}/shop?sort=newest`, label: "সব দেখুন — See all" };
    default:
      return null;
  }
}

function HeroSection({
  section,
  context,
}: {
  section: ResolvedSection;
  context: SectionViewContext;
}) {
  const hero = section.hero;
  if (!hero) return null;
  const { theme, accent } = context;
  return (
    <>
      {hero.announcement && (
        <p
          className="announcement-bar"
          style={accent ? { background: accent, color: "#fff" } : undefined}
        >
          {hero.announcement}
        </p>
      )}
      {theme !== "minimal" && hero.image_url && (
        // eslint-disable-next-line @next/next/no-img-element
        <img
          className={
            theme === "bold"
              ? "storefront-banner storefront-banner--large"
              : "storefront-banner"
          }
          src={hero.image_url}
          alt=""
        />
      )}
      {hero.is_customised && (
        <div className="storefront-hero-copy">
          {hero.headline && (
            <h2 className="storefront-hero-title">{hero.headline}</h2>
          )}
          {hero.subtext && (
            <p className="storefront-hero-subtext">{hero.subtext}</p>
          )}
          {hero.link_url && (
            <p>
              <Link
                className="storefront-hero-cta"
                href={hero.link_url}
                onClick={() => context.onSectionClick?.(section.id)}
              >
                {hero.link_label ?? "কিনুন — Shop now"}
              </Link>
            </p>
          )}
        </div>
      )}
    </>
  );
}

function RichTextSection({ section }: { section: ResolvedSection }) {
  const heading = cfgStr(section.config, "heading");
  const body = cfgStr(section.config, "body");
  if (!body) return null;
  return (
    <section className="store-text-block">
      {heading && <h2>{heading}</h2>}
      <p className="store-long-text">{body}</p>
    </section>
  );
}

function ProductRailSection({
  section,
  context,
}: {
  section: ResolvedSection;
  context: SectionViewContext;
}) {
  const products = section.products ?? [];
  if (products.length === 0) return null;
  const source = cfgStr(section.config, "source") ?? "newest";
  const title =
    cfgStr(section.config, "title") ??
    RAIL_DEFAULT_TITLES[source] ??
    RAIL_DEFAULT_TITLES.newest;
  const link = railLink(context.slug, source);
  return (
    <ProductRail
      title={title}
      products={products}
      linkHref={link?.href}
      linkLabel={link?.label}
    />
  );
}

function CollectionShelfSection({
  section,
  context,
}: {
  section: ResolvedSection;
  context: SectionViewContext;
}) {
  const { slug } = context;
  if (section.collection) {
    const products = section.products ?? [];
    if (products.length === 0) return null;
    return (
      <ProductRail
        title={cfgStr(section.config, "title") ?? section.collection.title}
        products={products}
        linkHref={`/${slug}/collections/${section.collection.slug}`}
        linkLabel="সব দেখুন — See all"
      />
    );
  }
  const collections = section.collections ?? [];
  if (collections.length === 0) return null;
  return (
    <section className="store-collections">
      <h2>{cfgStr(section.config, "title") ?? "স্টোরের কালেকশন — Collections"}</h2>
      <div className="store-collection-tiles">
        {collections.map((collection) => (
          <Link
            key={collection.id}
            className="store-collection-tile"
            href={`/${slug}/collections/${collection.slug}`}
          >
            {collection.cover_image_url && (
              <ProductImage
                src={collection.cover_image_url}
                alt=""
                className="store-collection-cover"
              />
            )}
            <span className="store-collection-title">{collection.title}</span>
            <span className="store-collection-count">
              {collection.product_count.toLocaleString("en-IN")}টি পণ্য
            </span>
          </Link>
        ))}
      </div>
    </section>
  );
}

function CategoryGridSection({
  section,
  context,
}: {
  section: ResolvedSection;
  context: SectionViewContext;
}) {
  const tiles = section.categories ?? [];
  if (tiles.length === 0) return null;
  const { slug } = context;
  const mode = cfgStr(section.config, "mode") ?? "taxonomy";
  if (mode === "labels") {
    return (
      <section className="store-categories">
        <h2>
          {cfgStr(section.config, "title") ??
            "ক্যাটাগরি অনুযায়ী কিনুন — Shop by category"}
        </h2>
        <div className="store-category-tiles">
          {tiles.map((tile) => (
            <Link
              key={tile.category ?? tile.name}
              className="store-category-tile"
              href={`/${slug}/shop?category=${encodeURIComponent(tile.category ?? tile.name)}`}
            >
              <span className="store-category-name">{tile.name}</span>
              <span className="store-category-count">
                {tile.product_count.toLocaleString("en-IN")}টি পণ্য
              </span>
            </Link>
          ))}
        </div>
      </section>
    );
  }
  return (
    <section className="store-specialties">
      <h2>
        {cfgStr(section.config, "title") ??
          "এই স্টোরের বিশেষত্ব — This store specialises in"}
      </h2>
      <div className="store-category-tiles">
        {tiles.map((tile) => (
          <Link
            key={tile.slug ?? tile.name}
            className="store-category-tile"
            href={`/${slug}/categories/${tile.slug ?? ""}`}
          >
            <span className="store-category-name">
              <CategoryIcon iconKey={tile.icon_key ?? "tag"} />{" "}
              {tile.name_bn ?? tile.name_en ?? tile.name}
            </span>
            <span className="store-category-count">
              {tile.product_count.toLocaleString("en-IN")}টি পণ্য
            </span>
            {tile.path.length > 1 && (
              <span className="store-category-path">
                {tile.path
                  .slice(0, -1)
                  .map((entry) => entry.name_en)
                  .join(" › ")}
              </span>
            )}
          </Link>
        ))}
      </div>
    </section>
  );
}

function ProductGridSection({
  section,
  context,
}: {
  section: ResolvedSection;
  context: SectionViewContext;
}) {
  return (
    <>
      <h2>{cfgStr(section.config, "title") ?? "সব পণ্য — All products"}</h2>
      <ProductGrid storeSlug={context.slug} layout={context.gridLayout} />
    </>
  );
}

function ImageBannerSection({
  section,
  context,
}: {
  section: ResolvedSection;
  context: SectionViewContext;
}) {
  const imageUrl = cfgStr(section.config, "image_url");
  if (!imageUrl) return null;
  const linkUrl = cfgStr(section.config, "link_url");
  const caption = cfgStr(section.config, "caption");
  const image = (
    // eslint-disable-next-line @next/next/no-img-element
    <img className="storefront-image-banner" src={imageUrl} alt={caption ?? ""} />
  );
  return (
    <figure className="storefront-banner-figure">
      {linkUrl ? (
        <Link href={linkUrl} onClick={() => context.onSectionClick?.(section.id)}>
          {image}
        </Link>
      ) : (
        image
      )}
      {caption && <figcaption>{caption}</figcaption>}
    </figure>
  );
}

function StoreInfoSection({ section }: { section: ResolvedSection }) {
  const info = section.store_info;
  if (!info || (!info.about && !info.return_policy)) return null;
  return (
    <>
      {info.about && (
        <section className="store-text-block">
          <h2>স্টোর সম্পর্কে — About this store</h2>
          <p className="store-long-text">{info.about}</p>
        </section>
      )}
      {info.return_policy && (
        <section className="store-text-block">
          <h2>রিটার্ন নীতি — Return policy</h2>
          <p className="store-long-text">{info.return_policy}</p>
        </section>
      )}
    </>
  );
}

function FaqSection({ section }: { section: ResolvedSection }) {
  const items = section.faq_items ?? [];
  if (items.length === 0) return null;
  return (
    <section className="storefront-faq">
      <h2>সাধারণ প্রশ্ন — Frequently asked questions</h2>
      {items.map((item, index) => (
        <details key={index} className="storefront-faq-item">
          <summary>{item.question}</summary>
          <p className="store-long-text">{item.answer}</p>
        </details>
      ))}
    </section>
  );
}

/**
 * Render ONE resolved section by type. Unknown (future)
 * types render nothing — the API already drops them from
 * the public model, and the editor preview does the
 * same, so this is a belt-and-braces null.
 */
export default function StorefrontSectionView({
  section,
  context,
}: {
  section: ResolvedSection;
  context: SectionViewContext;
}) {
  switch (section.type) {
    case "hero":
      return <HeroSection section={section} context={context} />;
    case "rich_text":
      return <RichTextSection section={section} />;
    case "product_rail":
      return <ProductRailSection section={section} context={context} />;
    case "collection_shelf":
      return <CollectionShelfSection section={section} context={context} />;
    case "category_grid":
      return <CategoryGridSection section={section} context={context} />;
    case "product_grid":
      return <ProductGridSection section={section} context={context} />;
    case "image_banner":
      return <ImageBannerSection section={section} context={context} />;
    case "store_info":
      return <StoreInfoSection section={section} />;
    case "faq":
      return <FaqSection section={section} />;
    default:
      return null;
  }
}
