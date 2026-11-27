import type { Metadata } from "next";
import Link from "next/link";
import BrowseResults from "@/components/BrowseResults";

/**
 * A category landing page (Wave 53): the category as its
 * own shareable, indexable shelf — the shared faceted
 * BrowseResults pinned to this category by the route,
 * with every other filter (price, rating, stock, sale,
 * stores) still available around it. The heading and
 * metadata name the category; the tiles on /categories
 * and the home page link here through /browse, and this
 * route is the canonical landing the metadata describes.
 */
export async function generateMetadata({
  params,
}: {
  params: { category: string };
}): Promise<Metadata> {
  const category = decodeURIComponent(params.category);
  return {
    title: `${category} — BD Marketplace`,
    description: `${category} ক্যাটাগরির সব পণ্য, দাম ও রেটিংসহ — All ${category} products with prices and ratings on BD Marketplace.`,
  };
}

export default function CategoryLandingPage({
  params,
}: {
  params: { category: string };
}) {
  const category = decodeURIComponent(params.category);
  const breadcrumbJsonLd = {
    "@context": "https://schema.org",
    "@type": "BreadcrumbList",
    itemListElement: [
      { "@type": "ListItem", position: 1, name: "হোম — Home", item: "/" },
      {
        "@type": "ListItem",
        position: 2,
        name: "ক্যাটাগরি — Categories",
        item: "/categories",
      },
      {
        "@type": "ListItem",
        position: 3,
        name: category,
        item: `/categories/${params.category}`,
      },
    ],
  };
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(breadcrumbJsonLd) }}
      />
      <p>
        <Link href="/categories">← সব ক্যাটাগরি — All categories</Link>
      </p>
      <BrowseResults
        basePath={`/categories/${params.category}`}
        heading={`ক্যাটাগরি: ${category}`}
        forcedCategory={category}
      />
    </>
  );
}
