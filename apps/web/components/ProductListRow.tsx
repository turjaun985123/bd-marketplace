import Link from "next/link";
import ProductImage from "./ProductImage";
import Stars from "./Stars";
import { Badge } from "./ui";
import type { Product } from "@/lib/api";
import { formatBdt } from "@/lib/format";

/**
 * The browse list-view row (Wave 53): one product as a
 * horizontal strip — thumbnail, title, store, rating and
 * sold line, and the price block — for shoppers who scan
 * faster in rows than in a grid. The price block follows
 * the card's honesty rules exactly (the flash price IS
 * the price with the base struck through; otherwise a
 * compare-at above the price is the only discount source;
 * no invented history, no invented drops).
 */
export default function ProductListRow({
  product,
  onAdd,
  added = false,
}: {
  product: Product;
  /** When given (signed-in browse), the row carries the add-to-cart action. */
  onAdd?: (productId: string) => void;
  added?: boolean;
}) {
  const salePrice = product.flash_price_poisha ?? null;
  const effectivePrice = salePrice ?? product.price_poisha;
  const discountBase =
    salePrice !== null ? product.price_poisha : (product.compare_at_poisha ?? null);
  const dropPercent =
    discountBase !== null && discountBase > effectivePrice
      ? Math.floor(((discountBase - effectivePrice) * 100) / discountBase)
      : 0;

  return (
    <article className="product-row card">
      <Link
        href={`/products/${product.id}`}
        className="product-row-image-link"
        aria-label={product.title}
      >
        <ProductImage
          src={product.image_url}
          alt={product.title}
          placeholderText={product.title}
          className="product-row-image"
        />
      </Link>
      <div className="product-row-body">
        <h3 className="product-row-title">
          <Link href={`/products/${product.id}`}>
            {product.title_bn ?? product.title}
          </Link>
        </h3>
        <p className="product-row-store">
          <Link href={`/${product.store_slug}`}>{product.store_slug}</Link>
          {product.category ? <> · {product.category}</> : null}
        </p>
        {(product.rating_count > 0 || product.sold_count > 0) && (
          <p className="product-row-meta">
            {product.rating_count > 0 && (
              <span>
                <Stars value={product.rating_avg} /> {product.rating_avg} (
                {product.rating_count})
              </span>
            )}
            {product.sold_count > 0 && (
              <span> · {product.sold_count} বিক্রি — sold</span>
            )}
          </p>
        )}
        <p className="product-row-stock">
          {product.stock > 0 ? (
            <span className="product-row-instock">স্টকে আছে — In stock</span>
          ) : (
            <span className="product-row-outstock">স্টক শেষ — Out of stock</span>
          )}
        </p>
      </div>
      <div className="product-row-price">
        {product.is_promoted && (
          <span className="sponsored-badge">স্পন্সরড — Sponsored</span>
        )}
        {salePrice !== null && <Badge tone="flash">ফ্ল্যাশ সেল — Flash sale</Badge>}
        <p className="product-row-price-now">{formatBdt(effectivePrice)}</p>
        {dropPercent > 0 && discountBase !== null && (
          <p className="product-row-price-was">
            {formatBdt(discountBase)} · -{dropPercent}%
          </p>
        )}
        {onAdd && product.stock > 0 && (
          <button type="button" onClick={() => onAdd(product.id)}>
            কার্টে যোগ করুন — Add to cart
          </button>
        )}
        {added && (
          <span>
            যোগ হয়েছে ✓ <Link href="/cart">কার্ট দেখুন</Link>
          </span>
        )}
      </div>
    </article>
  );
}
