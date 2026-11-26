import MessageStoreButton from "@/components/MessageStoreButton";
import ProductGrid from "@/components/ProductGrid";

/**
 * Per-seller storefront: the store's own page, rendering the SAME
 * shared ProductGrid as the central marketplace (scoped to this
 * store) — the same products, cards, ratings and cart behaviour,
 * never a forked copy. In production this route group is also served
 * via subdomain rewrite, e.g. `{store}.shop.example.bd`.
 */
export default function StorefrontPage({
  params,
}: {
  params: { store: string };
}) {
  return (
    <main>
      <h1>স্টোর: {params.store}</h1>
      <p>
        এটি <strong>{params.store}</strong>-এর নিজস্ব স্টোরফ্রন্ট — একই পণ্য
        কেন্দ্রীয় মার্কেটপ্লেসেও পাওয়া যায়।
      </p>
      <MessageStoreButton slug={params.store} />
      <h2>পণ্য — Products</h2>
      <ProductGrid storeSlug={params.store} />
    </main>
  );
}
