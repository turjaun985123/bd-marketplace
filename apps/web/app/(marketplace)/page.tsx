import Link from "next/link";
import DeliveryChargeWidget from "@/components/DeliveryChargeWidget";
import ProductGrid from "@/components/ProductGrid";

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
        <Link href="/orders">আমার অর্ডার — My orders</Link>
      </p>

      <h2>পণ্য — Products</h2>
      <ProductGrid />

      <DeliveryChargeWidget />
    </main>
  );
}
