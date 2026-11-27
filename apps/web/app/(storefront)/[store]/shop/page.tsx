import BrowseResults from "@/components/BrowseResults";
import StoreShopHeader from "@/components/StoreShopHeader";

/**
 * A store's shop page (Wave 53): the storefront's full
 * catalogue under the ONE discovery path — the shared
 * faceted BrowseResults pinned to this store by the
 * route, so the store's shoppers get the same filters
 * (its categories, its price span, ratings, stock and
 * sale flags), sorts and pagination as the marketplace,
 * scoped to this store's products only. The store filter
 * group is hidden: the scope is the page itself. The
 * store's identity rides above in StoreShopHeader.
 */
export default function StoreShopPage({
  params,
}: {
  params: { store: string };
}) {
  return (
    <>
      <StoreShopHeader slug={params.store} />
      <BrowseResults
        basePath={`/${params.store}/shop`}
        heading="এই স্টোরের সব পণ্য — Shop this store"
        forcedStoreSlug={params.store}
      />
    </>
  );
}
