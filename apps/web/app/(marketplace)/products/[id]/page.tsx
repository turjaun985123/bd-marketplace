"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import DeliveryEstimate from "@/components/DeliveryEstimate";
import FollowButton from "@/components/FollowButton";
import HelpfulButton from "@/components/HelpfulButton";
import StoreCollections from "@/components/StoreCollections";
import PriceTag from "@/components/PriceTag";
import ProductGallery from "@/components/ProductGallery";
import ProductImage from "@/components/ProductImage";
import ProductRail from "@/components/ProductRail";
import RatingSummary from "@/components/RatingSummary";
import RecentlyViewedRail from "@/components/RecentlyViewedRail";
import ShareRow from "@/components/ShareRow";
import Stars from "@/components/Stars";
import StoreStats from "@/components/StoreStats";
import StickyBuyBar from "@/components/StickyBuyBar";
import VerificationBadge from "@/components/VerificationBadge";
import WishlistButton from "@/components/WishlistButton";
import { Badge, Countdown } from "@/components/ui";
import { formatBdt } from "@/lib/format";
import {
  askQuestion,
  attachReviewImage,
  createConversation,
  createReview,
  deleteReview,
  deleteReviewImage,
  getAlsoBought,
  getProduct,
  getProductQuestions,
  getProductReviews,
  getStore,
  searchProducts,
  replyToReview,
  updateReply,
  updateReview,
  type AlsoBoughtItem,
  type Product,
  type ProductDetail,
  type ProductQuestion,
  type ProductVariant,
  type PublicStore,
  type QuestionSort,
  type Review,
  type ReviewSort,
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
 *
 * Wave 33: the variant picker — when the detail carries
 * variants, the buyer picks one (the first is preselected);
 * the shown price and stock become the variant's FINAL
 * price and its own stock, and add-to-cart sends the
 * variant id (the API refuses a variant-less add for a
 * variant product).
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
  const [product, setProduct] = useState<ProductDetail | null>(null);
  const [selectedVariantId, setSelectedVariantId] = useState<string | null>(
    null
  );
  const [reviews, setReviews] = useState<Review[] | null>(null);
  // Wave 54: the review list's sort (the API validates
  // the same keys); changing it refetches ONLY the list.
  const [reviewSort, setReviewSort] = useState<ReviewSort>("newest");
  const [related, setRelated] = useState<Product[]>([]);
  const [alsoBought, setAlsoBought] = useState<AlsoBoughtItem[]>([]);
  // Wave 54: rails from the product's own store.
  const [sameStore, setSameStore] = useState<Product[]>([]);
  const [store, setStore] = useState<PublicStore | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [added, setAdded] = useState(false);

  const [rating, setRating] = useState(5);
  const [body, setBody] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const [questions, setQuestions] = useState<ProductQuestion[] | null>(null);
  // Wave 54 extension: the Q&A sort + the unanswered
  // count the API reports alongside the list.
  const [questionSort, setQuestionSort] = useState<QuestionSort>("newest");
  const [unansweredCount, setUnansweredCount] = useState(0);
  const [questionBody, setQuestionBody] = useState("");
  const [questionError, setQuestionError] = useState<string | null>(null);
  const [questionSubmitting, setQuestionSubmitting] = useState(false);

  // Wave 54: the review list alone, under the current
  // sort — the sort control refetches this, not the page.
  const loadReviews = useCallback(
    async (sort: ReviewSort) => {
      const list = await getProductReviews(params.id, { sort });
      setReviews(list.items);
    },
    [params.id]
  );

  const reload = useCallback(async () => {
    const [p, list] = await Promise.all([
      getProduct(params.id, token),
      getProductReviews(params.id, { sort: reviewSort }),
    ]);
    setProduct(p);
    // Wave 33: preselect the first variant (picker order),
    // keeping a still-valid earlier pick across reloads.
    setSelectedVariantId((current) =>
      current && p.variants.some((v) => v.id === current)
        ? current
        : (p.variants[0]?.id ?? null)
    );
    setReviews(list.items);
    // Wave 25: the public Q&A list — best-effort like the
    // discovery rails, a failed read hides the section's list
    // instead of breaking the page.
    const qs = await getProductQuestions(params.id, {
      sort: questionSort,
    }).catch(() => null);
    if (qs) {
      setQuestions(qs.items);
      setUnansweredCount(qs.unanswered_count);
    }
    // The store is public data: it carries the owner check AND
    // the Wave 21 verification tier for the trust badge, which
    // every visitor should see — not only signed-in users.
    // Wave 54: the read is the public store view (follower
    // count + the viewer's follow state ride along; the
    // token makes `is_following` the viewer's own).
    const s = await getStore(p.store_slug, token).catch(() => null);
    setStore(s);
    // Discovery rails: public endpoints, best-effort — a
    // failed rail hides itself, it never breaks the page.
    // Wave 53: the same-category rail runs on the ONE
    // discovery path — the faceted browse, this category,
    // best sellers first, self excluded — instead of the
    // old related endpoint.
    const [rel, also, same] = await Promise.all([
      p.category
        ? searchProducts({
            categories: [p.category],
            sort: "sold",
            limit: 12,
          }).catch(() => null)
        : Promise.resolve(null),
      getAlsoBought(params.id).catch(() => null),
      // Wave 54: the same store's best sellers, this
      // product excluded — the "more from this seller"
      // rail under the detail.
      searchProducts({
        stores: [p.store_slug],
        sort: "sold",
        limit: 12,
      }).catch(() => null),
    ]);
    if (rel) {
      setRelated(rel.items.filter((item) => item.id !== p.id));
    }
    if (also) setAlsoBought(also.items);
    if (same) {
      setSameStore(same.items.filter((item) => item.id !== p.id));
    }
  }, [params.id, token, user, reviewSort, questionSort]);

  // Wave 54 extension: a Q&A sort change refetches only
  // the question list.
  async function onQuestionSortChange(next: QuestionSort) {
    setQuestionSort(next);
    const qs = await getProductQuestions(params.id, { sort: next }).catch(
      () => null
    );
    if (qs) {
      setQuestions(qs.items);
      setUnansweredCount(qs.unanswered_count);
    }
  }

  // Wave 24: record the view in the DEVICE-LOCAL history
  // (localStorage only — see useRecentlyViewed). Wave 53:
  // the record carries the minimal card snapshot, so the
  // rail renders without a fetch per product.
  useEffect(() => {
    if (product) {
      recordRecentlyViewed({
        id: product.id,
        title: product.title_bn ?? product.title,
        store_slug: product.store_slug,
        price_poisha: product.price_poisha,
        flash_price_poisha: product.flash_price_poisha,
        image_url: product.image_url,
      });
    }
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
    await add(params.id, 1, selectedVariantId);
    setAdded(true);
  }

  // Wave 54: buy now = add this item, then straight to
  // the cart to review and check out. The cart is the
  // platform's one purchase path — buy-now never opens a
  // parallel checkout, and anything already in the cart
  // stays visible there rather than being silently swept
  // into an order.
  async function onBuyNow() {
    await add(params.id, 1, selectedVariantId);
    router.push("/cart");
  }

  // Wave 54: a sort change refetches only the review list.
  function onReviewSortChange(next: ReviewSort) {
    setReviewSort(next);
    void loadReviews(next).catch(() => setReviews([]));
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

  // Wave 33: with a variant picked, the page speaks for the
  // variant — its final price and its own stock.
  const selectedVariant =
    product.variants.find((v) => v.id === selectedVariantId) ?? null;
  // Wave 52: a live flash price replaces the product's BASE
  // price on this page exactly as it does in the cart and at
  // checkout — the variant's delta composes on top of the
  // sale base (the API's per-variant final prices are
  // base-derived, so during a sale the page composes them
  // itself, through the same rule: delta added, a
  // non-positive result falls back to the stored final).
  const flashPrice = product.flash_price_poisha;
  const effectiveBase = flashPrice ?? product.price_poisha;
  const variantPrice = (variant: ProductVariant) =>
    flashPrice !== null && flashPrice + variant.price_delta_poisha > 0
      ? flashPrice + variant.price_delta_poisha
      : variant.final_price_poisha;
  const displayPrice = selectedVariant
    ? variantPrice(selectedVariant)
    : effectiveBase;
  const displayStock = selectedVariant ? selectedVariant.stock : product.stock;
  const dropPercent =
    flashPrice !== null && product.price_poisha > flashPrice
      ? Math.floor(
          ((product.price_poisha - flashPrice) * 100) / product.price_poisha
        )
      : product.compare_at_poisha !== null &&
          product.compare_at_poisha > displayPrice
        ? Math.floor(
            ((product.compare_at_poisha - displayPrice) * 100) /
              product.compare_at_poisha
          )
        : 0;

  return (
    <main>
      {/* Wave 54: real breadcrumbs — home, the product's
          category landing, its store, and the product
          itself (the current page, unlinked). Wave 56:
          a taxonomy-linked product walks its full
          category_path (each node to its own landing);
          an unlinked product keeps the label link. */}
      <nav aria-label="breadcrumb" className="breadcrumbs">
        <Link href="/">মার্কেটপ্লেস — Home</Link>
        {product.category_path.length > 0 ? (
          product.category_path.map((entry) => (
            <span key={entry.id}>
              {" › "}
              <Link href={`/categories/${entry.slug}`}>
                {entry.name_bn ?? entry.name_en}
              </Link>
            </span>
          ))
        ) : (
          product.category && (
            <>
              {" › "}
              <Link href={`/categories/${encodeURIComponent(product.category)}`}>
                {product.category}
              </Link>
            </>
          )
        )}
        {" › "}
        <Link href={`/${product.store_slug}`}>{store?.name ?? product.store_slug}</Link>
        {" › "}
        <span aria-current="page">{product.title_bn ?? product.title}</span>
        {store && (
          <>
            {" "}
            <VerificationBadge tier={store.verified_tier} />
          </>
        )}
      </nav>
      <h1>{product.title_bn ?? product.title}</h1>
      {product.title_bn && <p>{product.title}</p>}
      {/* Wave 51 extension: schema.org Product structured
          data — search engines render the photo, price and
          availability in rich results. Built from the same
          loaded product; image lists every gallery URL. */}
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{
          __html: JSON.stringify({
            "@context": "https://schema.org",
            "@type": "Product",
            name: product.title,
            description: product.description ?? undefined,
            image: product.images.map((image) => image.url_zoom),
            offers: {
              "@type": "Offer",
              priceCurrency: "BDT",
              // The price a buyer actually pays right now:
              // the flash price while a sale resolves.
              price: (effectiveBase / 100).toFixed(2),
              availability:
                product.stock_status === "in_stock"
                  ? "https://schema.org/InStock"
                  : "https://schema.org/OutOfStock",
            },
            ...(product.rating_count > 0
              ? {
                  aggregateRating: {
                    "@type": "AggregateRating",
                    ratingValue: product.rating_avg ?? 0,
                    reviewCount: product.rating_count,
                  },
                }
              : {}),
          }),
        }}
      />
      {/* Wave 54: the breadcrumb trail as structured data,
          mirroring the visible nav above it. */}
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{
          __html: JSON.stringify({
            "@context": "https://schema.org",
            "@type": "BreadcrumbList",
            itemListElement: [
              { "@type": "ListItem", position: 1, name: "Marketplace" },
              ...(product.category
                ? [
                    {
                      "@type": "ListItem",
                      position: 2,
                      name: product.category,
                    },
                  ]
                : []),
              {
                "@type": "ListItem",
                position: product.category ? 3 : 2,
                name: store?.name ?? product.store_slug,
              },
              {
                "@type": "ListItem",
                position: product.category ? 4 : 3,
                name: product.title,
              },
            ],
          }),
        }}
      />
      {/* Wave 51: the full image gallery (stage, thumbs,
          keyboard nav, lightbox) — or the placeholder tile
          while the product has no photos. Picking a variant
          focuses its assigned photo. */}
      <ProductGallery
        images={product.images}
        title={product.title_bn ?? product.title}
        focusUrl={selectedVariant?.image_url ?? null}
        variantId={selectedVariant?.id ?? null}
      />
      {product.variants.length > 0 && (
        <div>
          <p>আকার/রং বেছে নিন — Choose a variant:</p>
          <p className="variant-picker">
            {product.variants.map((variant) => (
              <button
                key={variant.id}
                type="button"
                className="variant-button"
                aria-pressed={variant.id === selectedVariantId}
                onClick={() => setSelectedVariantId(variant.id)}
              >
                {/* Wave 54: the variant's own photo (Wave
                    51) rides its button when one is
                    assigned, so colour/size picks are
                    visual, not just names. */}
                {variant.image_url && (
                  <ProductImage
                    src={variant.image_url}
                    alt=""
                    className="variant-button-thumb"
                  />
                )}
                {variant.id === selectedVariantId ? "✓ " : ""}
                {variant.name} —{" "}
                <PriceTag amountPoisha={variantPrice(variant)} />
                {variant.price_delta_poisha !== 0 && (
                  <span className="variant-delta">
                    {" "}
                    ({variant.price_delta_poisha > 0 ? "+" : "−"}
                    {formatBdt(Math.abs(variant.price_delta_poisha))})
                  </span>
                )}
                {variant.stock <= 0 ? " (স্টক শেষ)" : ""}
              </button>
            ))}
          </p>
        </div>
      )}
      {/* Wave 52 price block: while a flash sale resolves,
          the sale is the page's headline — badge, countdown
          and the struck base — and the price shown is the
          price the cart and checkout will charge. */}
      {flashPrice !== null && product.flash_ends_at && (
        <div className="flash-banner">
          <Badge tone="flash">ফ্ল্যাশ সেল — Flash sale</Badge>{" "}
          <span>শেষ হতে বাকি — Ends in</span>{" "}
          <Countdown endsAt={product.flash_ends_at} />
        </div>
      )}
      <p className="product-price-block">
        <span
          className={
            flashPrice !== null
              ? "product-price-now product-price-now--flash"
              : "product-price-now"
          }
        >
          {formatBdt(displayPrice)}
        </span>{" "}
        {flashPrice !== null ? (
          <s className="product-price-was">{formatBdt(product.price_poisha)}</s>
        ) : selectedVariant && selectedVariant.price_delta_poisha !== 0 ? (
          <span>
            (মূল দাম <PriceTag amountPoisha={product.price_poisha} />)
          </span>
        ) : null}{" "}
        {flashPrice === null && product.compare_at_poisha !== null && (
          <s>
            <PriceTag amountPoisha={product.compare_at_poisha} />
          </s>
        )}{" "}
        {dropPercent > 0 && <Badge tone="flash">-{dropPercent}%</Badge>}
      </p>
      <p className="product-meta-line">
        <Stars value={product.rating_avg} />{" "}
        {product.rating_count > 0 ? (
          <span>
            {product.rating_avg?.toFixed(1)} ({product.rating_count}টি রিভিউ)
          </span>
        ) : (
          <span>এখনো কোনো রিভিউ নেই</span>
        )}
        {product.sold_count > 0 && (
          <span>
            {" · "}
            {product.sold_count.toLocaleString("en-IN")}টি বিক্রি হয়েছে —{" "}
            {product.sold_count.toLocaleString("en-IN")} sold
          </span>
        )}
      </p>
      {/* Wave 54: the urgency line follows the API's own
          stock health — for the base product that is
          `stock_status` (the seller's low-stock threshold
          included, Wave 23); a picked variant has no
          threshold, so its line uses the small-number rule
          on the variant's own stock. The number shown is
          always the real stock. */}
      <p>
        {displayStock > 0
          ? (selectedVariant
              ? displayStock <= 5
              : product.stock_status === "low")
            ? `মাত্র ${displayStock}টি বাকি আছে — Only ${displayStock} left in stock`
            : `স্টকে আছে (${displayStock}টি)`
          : "স্টকে নেই"}
        {" · "}
        ওজন: {product.weight_grams} গ্রাম
      </p>
      {product.description && <p>{product.description}</p>}

      {/* Wave 54: the delivery block — the buyer's zone,
          the real charge for one unit, the published day
          range (see the component). */}
      <DeliveryEstimate weightGrams={product.weight_grams} />

      <p>
        <WishlistButton productId={product.id} />
      </p>
      <ShareRow
        title={product.title_bn ?? product.title}
        path={`/products/${product.id}`}
      />

      {token ? (
        <p>
          <button
            type="button"
            disabled={product.variants.length > 0 && displayStock <= 0}
            onClick={() => void onAdd()}
          >
            কার্টে যোগ করুন — Add to cart
          </button>{" "}
          <button
            type="button"
            disabled={product.variants.length > 0 && displayStock <= 0}
            onClick={() => void onBuyNow()}
          >
            এখনই কিনুন — Buy now
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

      {/* Wave 52: the seller card — who stands behind this
          product, with the store's own branding and the way
          to its storefront. Built from the same store read
          the verification badge uses. */}
      {store && (
        <section className="card store-card">
          {store.logo_url ? (
            // eslint-disable-next-line @next/next/no-img-element
            <img
              src={store.logo_url}
              alt=""
              className="featured-store-logo"
              loading="lazy"
            />
          ) : null}
          <div>
            <p className="store-card-name">
              <Link href={`/${store.slug}`}>{store.name}</Link>{" "}
              <VerificationBadge tier={store.verified_tier} />
            </p>
            {store.description ? <p>{store.description}</p> : null}
            {/* Wave 54: the store's live trust line +
                the follow button ride the seller card. */}
            <StoreStats store={store} />
            <p>
              <Link href={`/${store.slug}`}>
                স্টোরফ্রন্ট দেখুন — Visit the storefront
              </Link>{" "}
              <FollowButton
                slug={store.slug}
                storeOwnerId={store.owner_user_id}
                initialFollowing={store.is_following}
                initialCount={store.follower_count}
              />
            </p>
          </div>
        </section>
      )}

      <ProductRail
        title="একই ক্যাটাগরিতে আরও — More in this category"
        products={related}
      />
      {/* Renders nothing at all while the product has no
          co-purchases — the rail never pads itself. */}
      <ProductRail
        title="অন্যরা এটাও কিনেছেন — Others also bought"
        products={alsoBought}
      />
      {/* Wave 54: the rest of this seller's catalogue,
          best sellers first. */}
      <ProductRail
        title="এই স্টোরের আরও পণ্য — More from this store"
        products={sameStore}
        linkHref={store ? `/${store.slug}/shop?sort=sold` : undefined}
        linkLabel="স্টোরের সব পণ্য — All store products"
      />
      {/* Wave 54 extension: the seller's curated shelves
          for this store, as tiles to their public pages —
          renders nothing when the store has none. */}
      {store && <StoreCollections slug={store.slug} />}
      {/* Wave 53: the visitor's own device-local history,
          this product excluded — renders nothing when the
          history holds nothing else. */}
      <RecentlyViewedRail excludeId={params.id} />

      <h2>রিভিউ — Reviews</h2>

      {/* Wave 54: the distribution summary above the list
          (average, per-star bars, photo count). */}
      <RatingSummary productId={params.id} />

      {/* Wave 54: photographed reviews first-class — a
          strip of every review photo, each jumping to its
          review below. */}
      {reviews !== null && reviews.some((r) => r.image_url) && (
        <div className="review-photo-strip">
          {reviews
            .filter((r) => r.image_url)
            .map((r) => (
              <a key={r.id} href={`#review-${r.id}`}>
                <ProductImage
                  src={r.image_url ?? ""}
                  alt="রিভিউয়ের ছবি — Review photo"
                  className="review-photo-thumb"
                />
              </a>
            ))}
        </div>
      )}

      {reviews !== null && reviews.length > 0 && (
        <p>
          <label>
            সাজান — Sort{" "}
            <select
              value={reviewSort}
              onChange={(e) => onReviewSortChange(e.target.value as ReviewSort)}
            >
              <option value="newest">নতুন আগে — Newest</option>
              <option value="highest">বেশি রেটিং আগে — Highest</option>
              <option value="lowest">কম রেটিং আগে — Lowest</option>
              <option value="photos">ছবিসহ আগে — With photos</option>
              <option value="helpful">সবচেয়ে কাজের আগে — Most helpful</option>
            </select>
          </label>
        </p>
      )}

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

      {questions !== null && questions.length > 0 && (
        <p>
          <label>
            সাজান — Sort{" "}
            <select
              value={questionSort}
              onChange={(e) =>
                void onQuestionSortChange(e.target.value as QuestionSort)
              }
            >
              <option value="newest">নতুন আগে — Newest</option>
              <option value="answered">উত্তর দেওয়া আগে — Answered first</option>
            </select>
          </label>
          {unansweredCount > 0 &&
            ` · ${unansweredCount}টি প্রশ্ন উত্তরের অপেক্ষায় — awaiting the seller`}
        </p>
      )}

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

      {/* Wave 54: the sticky buy bar — the same price and
          the same actions, pinned once the buyer scrolls
          past the buy block. */}
      <StickyBuyBar
        title={product.title_bn ?? product.title}
        imageUrl={product.image_url}
        pricePoisha={displayPrice}
        wasPoisha={
          flashPrice !== null
            ? product.price_poisha
            : product.compare_at_poisha !== null &&
                product.compare_at_poisha > displayPrice
              ? product.compare_at_poisha
              : null
        }
        canBuy={displayStock > 0}
        signedIn={!!token}
        added={added && !cartError}
        onAdd={() => void onAdd()}
        onBuyNow={() => void onBuyNow()}
      />
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
  const [photoZoom, setPhotoZoom] = useState(false);

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
    <article className="card" id={`review-${review.id}`}>
      <p>
        <Stars value={review.rating} /> <strong>{review.buyer_display}</strong>{" "}
        <small>{new Date(review.created_at).toLocaleDateString("en-GB")}</small>
      </p>
      {review.body && <p>{review.body}</p>}
      {/* Wave 54 extension: the helpful vote. */}
      <p>
        <HelpfulButton
          reviewId={review.id}
          authorId={review.buyer_id}
          initialCount={review.helpful_count}
        />
      </p>
      {/* Wave 51: the reviewer's photo, when they attached one. */}
      {review.image_url && (
        <button
          type="button"
          className="review-photo-button"
          onClick={() => setPhotoZoom(true)}
          aria-label="রিভিউয়ের ছবি বড় করে দেখুন — Enlarge review photo"
        >
          <ProductImage
            src={review.image_url}
            alt="রিভিউয়ের ছবি — Review photo"
            className="review-photo"
          />
        </button>
      )}
      {photoZoom && review.image_url && (
        <div
          className="product-gallery-lightbox"
          role="dialog"
          aria-modal="true"
          aria-label="রিভিউয়ের ছবি — Review photo"
          onClick={() => setPhotoZoom(false)}
        >
          <button
            type="button"
            className="product-gallery-lightbox-close"
            aria-label="বন্ধ করুন — Close"
            onClick={() => setPhotoZoom(false)}
          >
            ✕
          </button>
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img
            src={review.image_url}
            alt="রিভিউয়ের ছবি — Review photo"
            className="product-gallery-lightbox-image"
          />
        </div>
      )}
      {isOwn && token && (
        <p className="review-photo-actions">
          <label>
            {review.image_url ? "ছবি বদলান — Replace photo" : "ছবি যোগ করুন — Add a photo"}{" "}
            <input
              type="file"
              accept="image/jpeg,image/png,image/webp"
              onChange={(event) => {
                const file = event.target.files?.[0];
                event.target.value = "";
                if (!file) return;
                setItemError(null);
                attachReviewImage(token, review.id, file)
                  .then(() => onChanged())
                  .catch((err) =>
                    setItemError(
                      err instanceof Error ? err.message : "ছবি যোগ করা যায়নি।"
                    )
                  );
              }}
            />
          </label>
          {review.image_url && (
            <button
              type="button"
              onClick={() => {
                setItemError(null);
                deleteReviewImage(token, review.id)
                  .then(() => onChanged())
                  .catch((err) =>
                    setItemError(
                      err instanceof Error ? err.message : "ছবি মোছা যায়নি।"
                    )
                  );
              }}
            >
              ছবি মুছুন — Remove photo
            </button>
          )}
        </p>
      )}

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
