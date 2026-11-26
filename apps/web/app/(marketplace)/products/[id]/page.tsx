"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import PriceTag from "@/components/PriceTag";
import Stars from "@/components/Stars";
import {
  createReview,
  deleteReview,
  getProduct,
  getProductReviews,
  getStore,
  replyToReview,
  updateReply,
  updateReview,
  type Product,
  type Review,
  type Store,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";
import { useCart } from "@/hooks/useCart";

/**
 * Product detail page (Wave 8): product info with the live rating
 * summary, the public review list (masked buyer display + seller
 * replies), and — for signed-in users — the review form. Review
 * eligibility is the API's call: when it rejects with 403, its
 * message ("You can review only products you received") is shown
 * as-is. The author gets edit/delete controls on their own review;
 * the store owner gets a reply box here (no separate seller page —
 * replies live where the reviews are read).
 */
export default function ProductDetailPage({
  params,
}: {
  params: { id: string };
}) {
  const { ready, token, user } = useAuth();
  const { add, error: cartError } = useCart();
  const [product, setProduct] = useState<Product | null>(null);
  const [reviews, setReviews] = useState<Review[] | null>(null);
  const [store, setStore] = useState<Store | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [added, setAdded] = useState(false);

  const [rating, setRating] = useState(5);
  const [body, setBody] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const reload = useCallback(async () => {
    const [p, list] = await Promise.all([
      getProduct(params.id, token),
      getProductReviews(params.id),
    ]);
    setProduct(p);
    setReviews(list.items);
    if (token && user) {
      const s = await getStore(p.store_slug).catch(() => null);
      setStore(s);
    }
  }, [params.id, token, user]);

  useEffect(() => {
    if (!ready) return;
    let cancelled = false;
    reload().catch((err) => {
      if (!cancelled) {
        setError(err instanceof Error ? err.message : "পণ্য লোড করা যায়নি।");
      }
    });
    return () => {
      cancelled = true;
    };
  }, [ready, reload]);

  const isStoreOwner =
    !!user && !!store && store.owner_user_id === user.id;

  async function onAdd() {
    setAdded(false);
    await add(params.id, 1);
    setAdded(true);
  }

  async function onSubmitReview(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setFormError(null);
    setSubmitting(true);
    try {
      await createReview(token, params.id, rating, body.trim() || undefined);
      setBody("");
      setRating(5);
      await reload();
    } catch (err) {
      // The API's own message (e.g. the 403 eligibility text) is the UI copy.
      setFormError(err instanceof Error ? err.message : "রিভিউ পাঠানো যায়নি।");
    } finally {
      setSubmitting(false);
    }
  }

  if (!ready || (product === null && error === null)) {
    return (
      <main>
        <p>লোড হচ্ছে… (Loading…)</p>
      </main>
    );
  }

  if (error || product === null) {
    return (
      <main>
        <h1>পণ্য পাওয়া যায়নি</h1>
        <p role="alert">{error ?? "এই পণ্যটি নেই।"}</p>
        <p>
          <Link href="/">মার্কেটপ্লেসে ফিরে যান</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <p>
        <Link href="/">← মার্কেটপ্লেস</Link>
        {" · "}
        <Link href={`/${product.store_slug}`}>স্টোর: {product.store_slug}</Link>
      </p>
      <h1>{product.title_bn ?? product.title}</h1>
      {product.title_bn && <p>{product.title}</p>}
      <p>
        <PriceTag amountPoisha={product.price_poisha} />{" "}
        {product.compare_at_poisha !== null && (
          <s>
            <PriceTag amountPoisha={product.compare_at_poisha} />
          </s>
        )}
      </p>
      <p>
        <Stars value={product.rating_avg} />{" "}
        {product.rating_count > 0 ? (
          <span>
            {product.rating_avg?.toFixed(1)} ({product.rating_count}টি রিভিউ)
          </span>
        ) : (
          <span>এখনো কোনো রিভিউ নেই</span>
        )}
      </p>
      <p>
        {product.stock > 0
          ? `স্টকে আছে (${product.stock}টি)`
          : "স্টকে নেই"}
        {" · "}
        ওজন: {product.weight_grams} গ্রাম
      </p>
      {product.description && <p>{product.description}</p>}

      {token ? (
        <p>
          <button type="button" onClick={() => void onAdd()}>
            কার্টে যোগ করুন — Add to cart
          </button>{" "}
          {added && !cartError && (
            <span>
              যোগ হয়েছে ✓ <Link href="/cart">কার্ট দেখুন</Link>
            </span>
          )}
        </p>
      ) : (
        <p>
          <Link href="/login">কিনতে লগইন করুন</Link>
        </p>
      )}
      {cartError && <p role="alert">{cartError}</p>}

      <h2>রিভিউ — Reviews</h2>

      {token ? (
        <form onSubmit={(e) => void onSubmitReview(e)}>
          <label htmlFor="rating">আপনার রেটিং</label>
          <select
            id="rating"
            value={rating}
            onChange={(e) => setRating(Number(e.target.value))}
          >
            {[5, 4, 3, 2, 1].map((n) => (
              <option key={n} value={n}>
                {n} ★
              </option>
            ))}
          </select>
          <label htmlFor="review-body">আপনার মতামত (ঐচ্ছিক)</label>
          <textarea
            id="review-body"
            value={body}
            onChange={(e) => setBody(e.target.value)}
            rows={3}
            cols={50}
            maxLength={1000}
          />
          <p>
            <button type="submit" disabled={submitting}>
              রিভিউ দিন — Submit review
            </button>
          </p>
          {formError && <p role="alert">{formError}</p>}
        </form>
      ) : (
        <p>
          রিভিউ দিতে <Link href="/login">লগইন করুন</Link>। শুধুমাত্র পণ্য
          হাতে পাওয়া ক্রেতারাই রিভিউ দিতে পারেন।
        </p>
      )}

      {reviews === null ? (
        <p>রিভিউ লোড হচ্ছে…</p>
      ) : reviews.length === 0 ? (
        <p>এখনো কোনো রিভিউ নেই — প্রথম রিভিউটি দিন।</p>
      ) : (
        reviews.map((review) => (
          <ReviewItem
            key={review.id}
            review={review}
            token={token}
            isOwn={!!user && user.id === review.buyer_id}
            isStoreOwner={isStoreOwner}
            ownStoreId={store?.id ?? null}
            onChanged={() => void reload()}
          />
        ))
      )}
    </main>
  );
}

function ReviewItem({
  review,
  token,
  isOwn,
  isStoreOwner,
  ownStoreId,
  onChanged,
}: {
  review: Review;
  token: string | null;
  isOwn: boolean;
  isStoreOwner: boolean;
  ownStoreId: string | null;
  onChanged: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [editRating, setEditRating] = useState(review.rating);
  const [editBody, setEditBody] = useState(review.body ?? "");
  const [replyText, setReplyText] = useState("");
  const [editingReply, setEditingReply] = useState(false);
  const [itemError, setItemError] = useState<string | null>(null);

  async function onSaveEdit() {
    if (!token) return;
    setItemError(null);
    try {
      await updateReview(token, review.id, {
        rating: editRating,
        body: editBody,
      });
      setEditing(false);
      onChanged();
    } catch (err) {
      setItemError(err instanceof Error ? err.message : "সংরক্ষণ করা যায়নি।");
    }
  }

  async function onDelete() {
    if (!token) return;
    setItemError(null);
    try {
      await deleteReview(token, review.id);
      onChanged();
    } catch (err) {
      setItemError(err instanceof Error ? err.message : "মুছে ফেলা যায়নি।");
    }
  }

  async function onSubmitReply() {
    if (!token) return;
    setItemError(null);
    try {
      if (editingReply) {
        await updateReply(token, review.id, replyText);
      } else {
        await replyToReview(token, review.id, replyText);
      }
      setReplyText("");
      setEditingReply(false);
      onChanged();
    } catch (err) {
      setItemError(err instanceof Error ? err.message : "উত্তর পাঠানো যায়নি।");
    }
  }

  const canEditReply =
    isStoreOwner && review.reply !== null && review.reply.store_id === ownStoreId;

  return (
    <article className="card">
      <p>
        <Stars value={review.rating} /> <strong>{review.buyer_display}</strong>{" "}
        <small>{new Date(review.created_at).toLocaleDateString("en-GB")}</small>
      </p>
      {review.body && <p>{review.body}</p>}

      {review.reply && !editingReply && (
        <div className="card">
          <p>
            <strong>বিক্রেতার উত্তর</strong>
          </p>
          <p>{review.reply.body}</p>
          {canEditReply && (
            <p>
              <button
                type="button"
                onClick={() => {
                  setReplyText(review.reply?.body ?? "");
                  setEditingReply(true);
                }}
              >
                উত্তর সম্পাদনা করুন
              </button>
            </p>
          )}
        </div>
      )}

      {isOwn && !editing && (
        <p>
          <button type="button" onClick={() => setEditing(true)}>
            সম্পাদনা
          </button>{" "}
          <button type="button" onClick={() => void onDelete()}>
            মুছে ফেলুন
          </button>
        </p>
      )}
      {isOwn && editing && (
        <div>
          <label>
            রেটিং{" "}
            <select
              value={editRating}
              onChange={(e) => setEditRating(Number(e.target.value))}
            >
              {[5, 4, 3, 2, 1].map((n) => (
                <option key={n} value={n}>
                  {n} ★
                </option>
              ))}
            </select>
          </label>
          <label>
            মতামত{" "}
            <textarea
              value={editBody}
              onChange={(e) => setEditBody(e.target.value)}
              rows={3}
              cols={50}
              maxLength={1000}
            />
          </label>
          <p>
            <button type="button" onClick={() => void onSaveEdit()}>
              সংরক্ষণ করুন
            </button>{" "}
            <button type="button" onClick={() => setEditing(false)}>
              বাতিল
            </button>
          </p>
        </div>
      )}

      {isStoreOwner && token && (review.reply === null || editingReply) && (
        <div>
          <label>
            বিক্রেতা হিসেবে উত্তর{" "}
            <textarea
              value={replyText}
              onChange={(e) => setReplyText(e.target.value)}
              rows={2}
              cols={50}
              maxLength={500}
            />
          </label>
          <p>
            <button
              type="button"
              disabled={replyText.trim().length === 0}
              onClick={() => void onSubmitReply()}
            >
              {editingReply ? "উত্তর সংরক্ষণ করুন" : "উত্তর দিন"}
            </button>{" "}
            {editingReply && (
              <button type="button" onClick={() => setEditingReply(false)}>
                বাতিল
              </button>
            )}
          </p>
        </div>
      )}

      {itemError && <p role="alert">{itemError}</p>}
    </article>
  );
}
