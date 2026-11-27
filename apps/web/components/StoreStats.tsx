import Link from "next/link";
import Stars from "./Stars";
import type { PublicStore } from "@/lib/api";

/**
 * The storefront trust line (Wave 54): the public store
 * view's live aggregates as one quiet row — the store's
 * rating over ALL its products' reviews (with the count,
 * and a link to the store's reviews page), its delivered
 * units, its follower count and its join date. Every
 * number comes from the API's aggregates; an unrated
 * store says so instead of showing empty stars as if
 * they were a score.
 */
export default function StoreStats({ store }: { store: PublicStore }) {
  const joined = new Date(store.created_at).toLocaleDateString("en-GB", {
    month: "short",
    year: "numeric",
  });
  return (
    <p className="store-stats">
      {store.rating_count > 0 ? (
        <span>
          <Stars value={store.rating_avg} /> {store.rating_avg?.toFixed(1)} (
          <Link href={`/${store.slug}/reviews`}>
            {store.rating_count}টি রিভিউ — reviews
          </Link>
          )
        </span>
      ) : (
        <span>এখনো কোনো রিভিউ নেই — No reviews yet</span>
      )}
      {store.total_sold > 0 && (
        <span>
          {" · "}
          {store.total_sold.toLocaleString("en-IN")}টি বিক্রি — sold
        </span>
      )}
      <span>
        {" · "}
        {store.follower_count.toLocaleString("en-IN")} জন ফলোয়ার — followers
      </span>
      <span>
        {" · "}
        {store.product_count.toLocaleString("en-IN")}টি পণ্য — products
      </span>
      <span> · যোগ দিয়েছেন — Joined {joined}</span>
    </p>
  );
}
