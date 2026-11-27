import Link from "next/link";
import AdminLink from "@/components/AdminLink";
import InstallAppButton from "@/components/InstallAppButton";
import DeliveryChargeWidget from "@/components/DeliveryChargeWidget";
import HomeDiscovery from "@/components/HomeDiscovery";
import HomeFeed from "@/components/HomeFeed";
import ProductGrid from "@/components/ProductGrid";
import SearchBar from "@/components/SearchBar";

/**
 * The marketplace home page. Wave 52 rebuilt its body: the
 * merchandised feed (HomeFeed — hero campaigns, category
 * tiles, the flash sale, collections, rails, featured
 * stores, all from GET /v1/home) now leads, the per-visitor
 * discovery strips follow, and the full catalogue grid —
 * the page's original content and its fallback when the
 * feed is unavailable — closes it. The header, account nav,
 * install affordance and search bar are unchanged.
 */
export default function MarketplaceHomePage() {
  return (
    <main>
      <h1>বিডি মার্কেটপ্লেস</h1>
      <p>
        বাংলাদেশের জন্য একটাই মার্কেটপ্লেস: প্রতিটা বিক্রেতা পায় নিজের
        স্টোরফ্রন্ট (Shopify-এর মতো), আর সব পণ্য থাকে একটাই কেন্দ্রীয়
        মার্কেটপ্লেসে (Amazon-এর মতো) — এক কার্ট, এক চেকআউট, ক্যাশ অন
        ডেলিভারি।
      </p>
      <p>
        <Link href="/login">লগইন / বিক্রেতা হোন — Login</Link>
        {" · "}
        <Link href="/cart">আমার কার্ট — Cart</Link>
        {" · "}
        <Link href="/wishlist">♡ উইশলিস্ট — Wishlist</Link>
        {" · "}
        <Link href="/browse">ব্রাউজ — Browse</Link>
        {" · "}
        <Link href="/saved-searches">সংরক্ষিত খোঁজ — Saved searches</Link>
        {" · "}
        <Link href="/orders">আমার অর্ডার — My orders</Link>
        {" · "}
        <Link href="/track">অর্ডার ট্র্যাক — Track order</Link>
        {" · "}
        <Link href="/messages">মেসেজ — Messages</Link>
        <AdminLink />
      </p>
      {/* Wave 38: the shared install affordance — renders
          only when the browser can actually install. */}
      <p>
        <InstallAppButton />
      </p>

      <SearchBar />

      <HomeFeed />

      <HomeDiscovery />

      <h2>সব পণ্য — All products</h2>
      <ProductGrid />

      <DeliveryChargeWidget />
    </main>
  );
}
