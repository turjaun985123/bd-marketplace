import Link from "next/link";
import PriceTag from "./PriceTag";
import ProductImage from "./ProductImage";
import WishlistButton from "./WishlistButton";
import { Badge } from "./ui";
import { formatBdt } from "@/lib/format";

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
  /**
   * Wave 51: the product's primary image URL (the API's
   * `image_url`). Null or omitted renders the initial-letter
   * placeholder tile — the card layout never depends on a
   * photo existing.
   */
  imageUrl?: string | null;
  /**
   * Wave 51 extension: the product's second image URL.
   * When set, the card cross-fades to it on hover (fine
   * pointers only) — the marketplace "see the back of the
   * shoe" gesture, with zero layout cost when absent.
   */
  hoverImageUrl?: string | null;
  /**
   * The seller's strikethrough comparison price (Wave 52):
   * when it exceeds the price, the card shows it struck
   * through with a -% badge. The flash price, when present,
   * takes precedence as the discount source.
   */
  compareAtPoisha?: number | null;
  /**
   * Units sold (Wave 52): the API's delivered-sub-orders
   * count. Rendered as the "X বিক্রি — sold" line only when
   * positive — a product with no completed sales shows no
   * sold line at all rather than a hollow "0 sold".
   */
  soldCount?: number;
  /**
   * The live flash sale price (Wave 52), when the API
   * resolved one for this product. The card prices at it,
   * strikes the base price, and badges the drop.
   */
  flashPricePoisha?: number | null;
  /** When the live flash sale ends (Wave 52); display only. */
  flashEndsAt?: string | null;
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
  imageUrl,
  hoverImageUrl,
  compareAtPoisha,
  soldCount,
  flashPricePoisha,
}: ProductCardProps) {
  // Wave 52 price block: the flash price, when the API
  // resolved one, IS the price — the base strikes through.
  // Otherwise a compare-at above the price is the discount
  // source. The percent is the same floor rule the API's
  // domain crate states (price_drop_percent).
  const salePrice = flashPricePoisha ?? null;
  const effectivePrice = salePrice ?? pricePoisha;
  const discountBase = salePrice !== null ? pricePoisha : (compareAtPoisha ?? null);
  const dropPercent =
    discountBase !== null && discountBase > effectivePrice
      ? Math.floor(((discountBase - effectivePrice) * 100) / discountBase)
      : 0;
  const image = (
    <span className="product-card-image-stack">
      <ProductImage
        src={imageUrl ?? null}
        alt={name}
        placeholderText={name}
        className="product-card-image"
      />
      {hoverImageUrl ? (
        <ProductImage
          src={hoverImageUrl}
          alt=""
          placeholderText={name}
          className="product-card-image product-card-image--hover"
        />
      ) : null}
    </span>
  );
  return (
    <article
      className={`product-card ${variant === "card" ? "card" : `card card--${variant}`}`}
    >
      {dropPercent > 0 && variant === "card" && (
        <span className="badge badge--flash product-card-discount">
          -{dropPercent}%
        </span>
      )}
      {href ? (
        <Link href={href} className="product-card-image-link" aria-label={name}>
          {image}
        </Link>
      ) : (
        image
      )}
      {(isPromoted || salePrice !== null) && (
        <p className="product-card-badges">
          {isPromoted && (
            <span className="sponsored-badge">স্পন্সরড — Sponsored</span>
          )}
          {salePrice !== null && <Badge tone="flash">ফ্ল্যাশ সেল — Flash sale</Badge>}
        </p>
      )}
      <h3 className="product-card-title">
        {href ? <Link href={href}>{name}</Link> : name}
      </h3>
      <p className="product-card-store">{storeName}</p>
      {salePrice !== null || dropPercent > 0 ? (
        <p className="product-card-price">
          <span
            className={
              salePrice !== null
                ? "product-card-price-now product-card-price-now--flash"
                : "product-card-price-now"
            }
          >
            {formatBdt(effectivePrice)}
          </span>
          <span className="product-card-price-was">
            {formatBdt(discountBase ?? pricePoisha)}
          </span>
        </p>
      ) : (
        <PriceTag amountPoisha={pricePoisha} />
      )}
      {((ratingCount !== undefined && ratingCount > 0) ||
        (soldCount !== undefined && soldCount > 0)) && (
        <p className="product-card-meta">
          {ratingCount !== undefined && ratingCount > 0 && (
            <span>
              ★ {(ratingAvg ?? 0).toFixed(1)} ({ratingCount})
            </span>
          )}
          {soldCount !== undefined && soldCount > 0 && (
            <span>
              {soldCount.toLocaleString("en-IN")} বিক্রি — sold
            </span>
          )}
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
