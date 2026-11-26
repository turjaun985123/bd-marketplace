import PriceTag from "./PriceTag";

export interface ProductCardProps {
  name: string;
  storeName: string;
  pricePoisha: number;
}

/**
 * Shared product card — built once, reused by the marketplace,
 * storefronts, seller dashboard and admin. Never copy-paste this markup
 * into a page; extend this component instead.
 */
export default function ProductCard({ name, storeName, pricePoisha }: ProductCardProps) {
  return (
    <article className="card">
      <h3>{name}</h3>
      <p>{storeName}</p>
      <PriceTag amountPoisha={pricePoisha} />
    </article>
  );
}
