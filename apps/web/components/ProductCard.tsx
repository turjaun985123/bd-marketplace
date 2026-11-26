import Link from "next/link";
import PriceTag from "./PriceTag";

export interface ProductCardProps {
  name: string;
  storeName: string;
  pricePoisha: number;
  /** When set, the title links to the product detail page. */
  href?: string;
  /** Wave 8 rating summary; the rating line shows only when count > 0. */
  ratingAvg?: number | null;
  ratingCount?: number;
}

/**
 * Shared product card — built once, reused by the marketplace,
 * storefronts, seller dashboard and admin. Never copy-paste this markup
 * into a page; extend this component instead.
 */
export default function ProductCard({
  name,
  storeName,
  pricePoisha,
  href,
  ratingAvg,
  ratingCount,
}: ProductCardProps) {
  return (
    <article className="card">
      <h3>{href ? <Link href={href}>{name}</Link> : name}</h3>
      <p>{storeName}</p>
      <PriceTag amountPoisha={pricePoisha} />
      {ratingCount !== undefined && ratingCount > 0 && (
        <p>
          ★ {(ratingAvg ?? 0).toFixed(1)} ({ratingCount})
        </p>
      )}
    </article>
  );
}
