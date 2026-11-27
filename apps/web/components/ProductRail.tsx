import ProductCard from "./ProductCard";
import { Rail, SectionHeader } from "./ui";
import type { Product } from "@/lib/api";

/**
 * A titled strip of product cards — the one rendering for
 * every discovery rail: related products and also-bought on
 * the product page, the personalised + recently-viewed
 * strips on the home page, and (Wave 52) every home-feed
 * rail. Renders NOTHING for an empty list, so a rail with
 * nothing honest to show simply does not appear.
 *
 * Wave 52: the rail is the design system's horizontal
 * scroll-snap row with a shared section header, and the
 * cards receive the full Wave 52 view data (sold counts,
 * flash prices, compare-at) from the shared Product type.
 */
export default function ProductRail({
  title,
  products,
  linkHref,
  linkLabel,
}: {
  title: string;
  products: Product[];
  linkHref?: string;
  linkLabel?: string;
}) {
  if (products.length === 0) return null;
  return (
    <section>
      <SectionHeader title={title} linkHref={linkHref} linkLabel={linkLabel} />
      <Rail cards>
        {products.map((p) => (
          <ProductCard
            key={p.id}
            productId={p.id}
            name={p.title_bn ?? p.title}
            storeName={p.store_slug}
            pricePoisha={p.price_poisha}
            href={`/products/${p.id}`}
            ratingAvg={p.rating_avg}
            ratingCount={p.rating_count}
            isPromoted={p.is_promoted}
            imageUrl={p.image_url}
            hoverImageUrl={p.hover_image_url}
            compareAtPoisha={p.compare_at_poisha}
            soldCount={p.sold_count}
            flashPricePoisha={p.flash_price_poisha}
            flashEndsAt={p.flash_ends_at}
          />
        ))}
      </Rail>
    </section>
  );
}
