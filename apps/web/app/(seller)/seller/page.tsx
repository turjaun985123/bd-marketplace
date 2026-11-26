import Link from "next/link";

export default function SellerDashboardPage() {
  return (
    <main>
      <h1>বিক্রেতা ড্যাশবোর্ড</h1>
      <p>
        <Link href="/seller/products">আমার পণ্য — My products</Link>
        {" · "}
        <Link href="/seller/orders">আমার বিক্রির অর্ডার — Seller orders</Link>
        {" · "}
        <Link href="/login">লগইন — Login</Link>
      </p>
      <p>
        Seller dashboard placeholder (Wave 1). মোবাইলেই চালানো যায় এমন সহজ
        ড্যাশবোর্ড: পণ্য যোগ করা, অর্ডার দেখা, কুরিয়ার বুকিং, পেআউট হিসাব —
        সবই আসবে পরের wave-গুলোতে, প্রতিটা কাজের জন্য আলাদা callable
        function দিয়ে।
      </p>
    </main>
  );
}
