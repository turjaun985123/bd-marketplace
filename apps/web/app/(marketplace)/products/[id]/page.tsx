"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import PriceTag from "@/components/PriceTag";
import ProductRail from "@/components/ProductRail";
import Stars from "@/components/Stars";
import VerificationBadge from "@/components/VerificationBadge";
import WishlistButton from "@/components/WishlistButton";
import {
  askQuestion,
  createConversation,
  createReview,
  deleteReview,
  getAlsoBought,
  getProduct,
  getProductQuestions,
  getProductReviews,
  getRelatedProducts,
  getStore,
  replyToReview,
  updateReply,
  updateReview,
  type AlsoBoughtItem,
  type Product,
  type ProductQuestion,
  type Review,
  type Store,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";
import { useCart } from "@/hooks/useCart";
import { recordRecentlyViewed } from "@/hooks/useRecentlyViewed";

/**
 * Product detail page (Wave 8): product info with the live rating
 * summary, the public review list (masked buyer display + seller
 * replies), and — for signed-in users — the review form. Review
 * eligibility is the API's call: when it rejects with 403, its
 * message ("You can review only products you received") is shown
 * as-is. The author gets edit/delete controls on their own review;
 * the store owner gets a reply box here (no separate seller page —
 * replies live where the reviews are read).
 *
 * Wave 24: the wishlist heart, the related + also-bought
 * discovery rails (the shared ProductRail — the also-bought
 * rail renders nothing while the product has no
 * co-purchases), and the device-local recently-viewed
 * recording.
 *
 * Wave 25: the Q&A section below the reviews — public
 * questions with the seller's public answers. Signed-in
 * non-owners ask here; the owner is pointed at the seller
 * Q&A inbox instead (the API refuses own-store questions).
 */
export default function ProductDetailPage({
  params,
}: {
  params: { id: string };
}) {
  const { ready, token, user } = useAuth();
  const { add, error: cartError } = useCart();
  const router = useRouter();
  const [messageError, setMessageError] = useState<string | null>(null);
  const [product, setProduct] = useState<Product | null>(null);
  const [reviews, setReviews] = useState<Review[] | null>(null);
  const [related, setRelated] = useState<Product[]>([]);
  const [alsoBought, setAlsoBought] = useState<AlsoBoughtItem[]>([]);
  const [store, setStore] = useState<Store | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [added, setAdded] = useState(false);

  const [rating, setRating] = useState(5);
  const [body, setBody] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const [questions, setQuestions] = useState<ProductQuestion[] | null>(null);
  const [questionBody, setQuestionBody] = useState("");
  const [questionError, setQuestionError] = useState<string | null>(null);
  const [questionSubmitting, setQuestionSubmitting] = useState(false);

  const reload = useCallback(async () => {
    const [p, list] = await Promise.all([
      getProduct(params.id, token),
      getProductReviews(params.id),
    ]);
    setProduct(p);
    setReviews(list.items);
    // Wave 25: the public Q&A list — best-effort like the
    // discovery rails, a failed read hides the section's list
    // instead of breaking the page.
    const qs = await getProductQuestions(params.id).catch(() => null);
    if (qs) setQuestions(qs.items);
    // The store is public data: it carries the owner check AND
    // the Wave 21 verification tier for the trust badge, which
    // every visitor should see — not only signed-in users.
    const s = await getStore(p.store_slug).catch(() => null);
    setStore(s);
    // Wave 24 discovery rails: public endpoints, best-effort —
    // a failed rail hides itself, it never breaks the page.
    const [rel, also] = await Promise.all([
      getRelatedProducts(params.id).catch(() => null),
      getAlsoBought(params.id).catch(() => null),
    ]);
    if (rel) setRelated(rel.items);
    if (also) setAlsoBought(also.items);
  }, [params.id, token, user]);

  // Wave 24: record the view in the DEVICE-LOCAL history
  // (localStorage only — see useRecentlyViewed).
  useEffect(() => {
    if (product) recordRecentlyViewed(product.id);
  }, [product]);

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

  // Ask the seller about THIS product (Wave 13): find-or-create
  // the product-scoped conversation and go to the thread.
  async function onMessageSeller() {
    if (!token || !product) return;
    setMessageError(null);
    try {
      const conversation = await createConversation(
        token,
        product.store_id,
        product.id
      );
      router.push(`/messages/${conversation.id}`);
    } catch (err) {
      setMessageError(
        err instanceof Error ? err.message : "মেসেজ শুরু করা যায়নি।"
      );
    }
  }

  async function onAskQuestion(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setQuestionError(null);
    setQuestionSubmitting(true);
    try {
      await askQuestion(token, params.id, questionBody.trim());
      setQuestionBody("");
      await reload();
    } catch (err) {
      setQuestionError(
        err instanceof Error ? err.message : "প্রশ্ন পাঠানো যায়নি।"
      );
    } finally {
      setQuestionSubmitting(false);
    }
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
        {store && (
          <>
            {" "}
            <VerificationBadge tier={store.verified_tier} />
          </>
        )}
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

      <p>
        <WishlistButton productId={product.id} />
      </p>

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

      {token && (
        <p>
          <button type="button" onClick={() => void onMessageSeller()}>
            মেসেজ করুন — Message the seller
          </button>
          {messageError && <span role="alert"> {messageError}</span>}
        </p>
      )}

      <ProductRail
        title="সম্পর্কিত পণ্য — Related products"
        products={related}
      />
      {/* Renders nothing at all while the product has no
          co-purchases — the rail never pads itself. */}
      <ProductRail
        title="অন্যরা এটাও কিনেছেন — Others also bought"
        products={alsoBought}
      />

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

      <h2>প্রশ্নোত্তর — Questions &amp; answers</h2>

      {isStoreOwner ? (
        <p>
          এটি আপনার স্টোরের পণ্য — ক্রেতাদের প্রশ্নের উত্তর দিন{" "}
          <Link href="/seller/questions">প্রশ্নোত্তর ইনবক্সে</Link>।
        </p>
      ) : token ? (
        <form onSubmit={(e) => void onAskQuestion(e)}>
          <label htmlFor="question-body">বিক্রেতাকে প্রশ্ন করুন</label>
          <textarea
            id="question-body"
            value={questionBody}
            onChange={(e) => setQuestionBody(e.target.value)}
            rows={2}
            cols={50}
            minLength={3}
            maxLength={500}
            required
          />
          <p>
            <button
              type="submit"
              disabled={questionSubmitting || questionBody.trim().length < 3}
            >
              প্রশ্ন করুন — Ask question
            </button>
          </p>
          {questionError && <p role="alert">{questionError}</p>}
        </form>
      ) : (
        <p>
          প্রশ্ন করতে <Link href="/login">লগইন করুন</Link>।
        </p>
      )}

      {questions === null ? (
        <p>প্রশ্ন লোড হচ্ছে…</p>
      ) : questions.length === 0 ? (
        <p>এখনো কোনো প্রশ্ন নেই — প্রথম প্রশ্নটি করুন।</p>
      ) : (
        questions.map((question) => (
          <article className="card" key={question.id}>
            <p>
              <strong>প্রশ্ন:</strong> {question.body}
            </p>
            <p>
              <small>
                {question.asker_display} ·{" "}
                {new Date(question.created_at).toLocaleDateString("en-GB")}
              </small>
            </p>
            {question.answer_body ? (
              <div className="card">
                <p>
                  <strong>বিক্রেতার উত্তর</strong>
                </p>
                <p>{question.answer_body}</p>
              </div>
            ) : (
              <p>উত্তরের অপেক্ষায় — Awaiting the seller&apos;s answer</p>
            )}
          </article>
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
