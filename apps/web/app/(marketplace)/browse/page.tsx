import type { Metadata } from "next";
import BrowseResults from "@/components/BrowseResults";

/**
 * The faceted browse (Wave 53): the marketplace's main
 * shopping surface — filter sidebar / mobile drawer,
 * sorts, grid and list views, windowed pagination, all
 * state in the URL. The heavy lifting is the shared
 * BrowseResults component, which /search renders too.
 * Metadata follows the URL: a category or text browse
 * names itself, so shared links and index entries say
 * what the shelf holds.
 */
export async function generateMetadata({
  searchParams,
}: {
  searchParams: { category?: string; q?: string };
}): Promise<Metadata> {
  const category = searchParams.category;
  const q = searchParams.q;
  if (category) {
    return {
      title: `${category} — Browse · BD Marketplace`,
      description: `${category} ক্যাটাগরির পণ্য — Browse ${category} products by price, rating and store.`,
    };
  }
  if (q) {
    return {
      title: `“${q}” — Search · BD Marketplace`,
      description: `“${q}” দিয়ে খোঁজা পণ্য — Products matching “${q}”, with filters for category, price and rating.`,
    };
  }
  return {
    title: "ব্রাউজ — Browse · BD Marketplace",
    description:
      "ক্যাটাগরি, স্টোর, দাম আর রেটিং দিয়ে পণ্য খুঁজুন — Browse products by category, store, price and rating.",
  };
}

export default function BrowsePage() {
  return <BrowseResults basePath="/browse" heading="ব্রাউজ — Browse" />;
}
