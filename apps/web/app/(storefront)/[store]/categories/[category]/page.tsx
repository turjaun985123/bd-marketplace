import StoreCategoryLanding from "@/components/StoreCategoryLanding";
import StoreShopHeader from "@/components/StoreShopHeader";

/**
 * A store's category page (Wave 56):
 * /<store>/categories/<segment> — the storefront's
 * specialisation tiles land here. The header keeps the
 * store's identity on the page; the landing resolves
 * the segment (taxonomy slug → this store's subtree
 * browse; legacy label → this store's text browse) —
 * see StoreCategoryLanding.
 */
export default function StoreCategoryPage({
  params,
}: {
  params: { store: string; category: string };
}) {
  const segment = decodeURIComponent(params.category);
  return (
    <>
      <StoreShopHeader slug={params.store} />
      <StoreCategoryLanding storeSlug={params.store} segment={segment} />
    </>
  );
}
