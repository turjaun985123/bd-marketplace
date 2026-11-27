import { ProductCardSkeleton } from "@/components/ui";

/**
 * The browse route's loading shell (Wave 53): the result
 * grid's skeleton while the page's first fetch is in
 * flight, in the same grid the results will occupy.
 */
export default function BrowseLoading() {
  return (
    <main>
      <h1>ব্রাউজ — Browse</h1>
      <div className="search-grid" aria-hidden="true">
        {Array.from({ length: 8 }, (_, i) => (
          <ProductCardSkeleton key={i} />
        ))}
      </div>
    </main>
  );
}
