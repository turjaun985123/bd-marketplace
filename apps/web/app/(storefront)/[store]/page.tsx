import StorefrontView from "@/components/StorefrontView";

/**
 * Per-seller storefront: the store's own page, rendered by the
 * ONE shared StorefrontView — the store's theme (Wave 18)
 * selects a layout variant of it, and the products are the same
 * shared ProductGrid as the central marketplace (scoped to this
 * store): same products, cards, ratings and cart behaviour,
 * never a forked copy. In production this route group is also
 * served via subdomain rewrite, e.g. `{store}.shop.example.bd`.
 */
export default function StorefrontPage({
  params,
}: {
  params: { store: string };
}) {
  return (
    <main>
      <StorefrontView slug={params.store} />
    </main>
  );
}
