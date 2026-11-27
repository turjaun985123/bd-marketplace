"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import HelpfulButton from "@/components/HelpfulButton";
import RatingSummary from "@/components/RatingSummary";
import Stars from "@/components/Stars";
import {
  listStoreReviews,
  type ReviewSort,
  type StoreReview,
} from "@/lib/api";

/**
 * A store's all-reviews page (Wave 54): every review of
 * every product the store sells, newest first (or by the
 * chosen sort), each naming its product and linking to
 * it, with the seller's reply inline where one exists.
 * The store-wide distribution summary rides on top — the
 * same aggregate the storefront's trust line quotes, so
 * the page and the headline can never disagree. Reviews
 * of since-drafted products stay listed: they were real
 * purchases, and hiding them would rewrite the store's
 * record.
 */
export default function StoreReviewsPage({
  params,
}: {
  params: { store: string };
}) {
  const slug = params.store;
  const [reviews, setReviews] = useState<StoreReview[] | null>(null);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [sort, setSort] = useState<ReviewSort>("newest");
  const [error, setError] = useState<string | null>(null);
  const LIMIT = 20;

  const reload = useCallback(async () => {
    try {
      const list = await listStoreReviews(slug, {
        page,
        limit: LIMIT,
        sort,
      });
      setReviews(list.items);
      setTotal(list.total);
      setError(null);
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "রিভিউ লোড করা যায়নি।"
      );
    }
  }, [slug, page, sort]);

  useEffect(() => {
    let cancelled = false;
    reload().catch(() => {
      if (!cancelled) setError("রিভিউ লোড করা যায়নি।");
    });
    return () => {
      cancelled = true;
    };
  }, [reload]);

  const pages = Math.max(1, Math.ceil(total / LIMIT));

  return (
    <main>
      <p>
        <Link href={`/${slug}`}>← স্টোরফ্রন্ট — Storefront</Link>
      </p>
      <h1>স্টোরের সব রিভিউ — All reviews</h1>
      <RatingSummary storeSlug={slug} />
      {error && <p role="alert">{error}</p>}
      <p>
        <label>
          সাজান — Sort{" "}
          <select
            value={sort}
            onChange={(e) => {
              setSort(e.target.value as ReviewSort);
              setPage(1);
            }}
          >
            <option value="newest">নতুন আগে — Newest</option>
            <option value="highest">বেশি রেটিং আগে — Highest</option>
            <option value="lowest">কম রেটিং আগে — Lowest</option>
            <option value="photos">ছবিসহ আগে — With photos</option>
            <option value="helpful">সবচেয়ে কাজের আগে — Most helpful</option>
          </select>
        </label>
      </p>
      {reviews === null ? (
        <p>লোড হচ্ছে…</p>
      ) : reviews.length === 0 ? (
        <p>এই স্টোরের কোনো পণ্যে এখনো রিভিউ পড়েনি।</p>
      ) : (
        reviews.map((review) => (
          <article key={review.id} className="card">
            <p>
              <Stars value={review.rating} />{" "}
              <strong>{review.buyer_display}</strong>{" "}
              <span>
                {new Date(review.created_at).toLocaleDateString("en-GB")}
              </span>
            </p>
            <p>
              পণ্য — Product:{" "}
              <Link href={`/products/${review.product_id}`}>
                {review.product_title}
              </Link>
            </p>
            {review.body && <p>{review.body}</p>}
            <p>
              <HelpfulButton
                reviewId={review.id}
                authorId={review.buyer_id}
                initialCount={review.helpful_count}
              />
            </p>
            {review.image_url && (
              // eslint-disable-next-line @next/next/no-img-element
              <img
                src={review.image_url}
                alt="রিভিউয়ের ছবি — Review photo"
                style={{ maxWidth: 240 }}
              />
            )}
            {review.reply && (
              <div className="card">
                <p>
                  <strong>বিক্রেতার জবাব</strong>{" "}
                  <span>
                    {new Date(review.reply.created_at).toLocaleDateString(
                      "en-GB"
                    )}
                  </span>
                </p>
                <p>{review.reply.body}</p>
              </div>
            )}
          </article>
        ))
      )}
      {pages > 1 && (
        <p>
          <button
            type="button"
            disabled={page <= 1}
            onClick={() => setPage(page - 1)}
          >
            ← আগের
          </button>{" "}
          পৃষ্ঠা {page} / {pages}{" "}
          <button
            type="button"
            disabled={page >= pages}
            onClick={() => setPage(page + 1)}
          >
            পরের →
          </button>
        </p>
      )}
    </main>
  );
}
