import BrowseResults from "@/components/BrowseResults";

/**
 * Search results (Wave 15's page, Wave 53's engine): the
 * same shared BrowseResults the /browse page renders —
 * one discovery path, no duplicated filter logic. Old
 * links keep working: every parameter the Wave 15 page
 * understood (q, store_slug, category, the price bounds,
 * sort, page) is part of the shared browse state, and
 * storefront within-store searches still arrive scoped
 * via store_slug.
 */
export default function SearchPage() {
  return <BrowseResults basePath="/search" heading="খুঁজুন — Search" />;
}
