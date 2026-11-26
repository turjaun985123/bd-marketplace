import DeliveryChargeWidget from "@/components/DeliveryChargeWidget";
import ProductCard from "@/components/ProductCard";

// Wave 1 placeholder data — replaced by Rust API product calls in Wave 2.
const demoProducts = [
  { name: "নমুনা পণ্য — Demo product", storeName: "নমুনা স্টোর", pricePoisha: 45000 },
];

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
        Central marketplace home (Wave 1 placeholder). Seller storefronts,
        unified cart and order splitting are powered by the Rust API.
      </p>

      <h2>পণ্য — Products</h2>
      {demoProducts.map((p) => (
        <ProductCard
          key={p.name}
          name={p.name}
          storeName={p.storeName}
          pricePoisha={p.pricePoisha}
        />
      ))}

      <DeliveryChargeWidget />
    </main>
  );
}
