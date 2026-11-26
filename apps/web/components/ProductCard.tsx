import Link from "next/link";
import PriceTag from "./PriceTag";
import WishlistButton from "./WishlistButton";

export interface ProductCardProps {
  name: string;
  storeName: string;
  pricePoisha: number;
  /** When set, the title links to the product detail page. */
  href?: string;
  /**
   * Wave 24: when the product id is known, the card carries
   * the wishlist heart toggle. Presentational callers that
   * only have display data (seller/admin previews) omit it.
   */
  productId?: string;
  /** Wave 8 rating summary; the rating line shows only when count > 0. */
  ratingAvg?: number | null;
  ratingCount?: number;
  /**
   * Wave 26: the product is inside a live sponsored-promotion
   * window. Sponsored placement is always labelled — the card
   * renders the স্পন্সরড badge whenever this is set.
   */
  isPromoted?: boolean;
  /**
   * Wave 18 storefront theme variant: "card" (default, the
   * marketplace look), "row" (minimal theme's compact list row),
   * "large" (bold theme's oversized card). Same content, three
   * presentations — the variant only changes the CSS class.
   */
  variant?: "card" | "row" | "large";
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
  productId,
  ratingAvg,
  ratingCount,
  isPromoted,
  variant = "card",
}: ProductCardProps) {
  return (
    <article className={variant === "card" ? "card" : `card card--${variant}`}>
      {isPromoted && (
        <p>
          <span className="sponsored-badge">স্পন্সরড — Sponsored</span>
        </p>
      )}
      <h3>{href ? <Link href={href}>{name}</Link> : name}</h3>
      <p>{storeName}</p>
      <PriceTag amountPoisha={pricePoisha} />
      {ratingCount !== undefined && ratingCount > 0 && (
        <p>
          ★ {(ratingAvg ?? 0).toFixed(1)} ({ratingCount})
        </p>
      )}
      {productId && (
        <p>
          <WishlistButton productId={productId} />
        </p>
      )}
    </article>
  );
}
