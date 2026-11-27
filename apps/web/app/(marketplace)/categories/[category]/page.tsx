import type { Metadata } from "next";
import Link from "next/link";
import CategoryLanding from "@/components/CategoryLanding";

/**
 * A category landing page (Wave 53; taxonomy in Wave 56):
 * the route segment is resolved by the CategoryLanding
 * client component — a taxonomy SLUG renders the node's
 * own page (children tiles + the subtree browse), any
 * other segment renders the Wave 53 free-text label
 * landing unchanged. The metadata stays segment-based:
 * the server cannot know which kind a segment is without
 * the API round-trip the resolver makes.
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
      <CategoryLanding segment={category} />
    </>
  );
}
