import ProductCard from "./ProductCard";
import type { Product } from "@/lib/api";

/**
 * A titled strip of product cards (Wave 24) — the one
 * rendering for every discovery rail: related products and
 * also-bought on the product page, and the personalised +
 * recently-viewed strips on the home page. Renders NOTHING
 * for an empty list, so a rail with nothing honest to show
 * simply does not appear.
 */
export default function ProductRail({
  title,
  products,
}: {
  title: string;
  products: Product[];
}) {
  if (products.length === 0) return null;
  return (
    <section>
      <h2>{title}</h2>
      <div>
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
          />
        ))}
      </div>
    </section>
  );
}
