"use client";

import { useMemo } from "react";
import Link from "next/link";
import CouponManager, { type CouponManagerApi } from "@/components/CouponManager";
import {
  createSellerCoupon,
  deleteSellerCoupon,
  listSellerCoupons,
  setSellerCouponActive,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Seller coupons (Wave 14): coupons for the seller's own store.
 * A store coupon discounts only this store's goods and the SELLER
 * bears the discount — commission is computed on the discounted
 * goods (see the README's Wave 14 money design). The form and the
 * list are the shared CouponManager; this page only wires the
 * seller endpoints into it.
 */
export default function SellerCouponsPage() {
  const { ready, token } = useAuth();

  const api = useMemo<CouponManagerApi | null>(() => {
    if (!token) return null;
    return {
      load: async () => (await listSellerCoupons(token, { limit: 50 })).items,
      create: (input) => createSellerCoupon(token, input),
      setActive: (id, active) => setSellerCouponActive(token, id, active),
      remove: (id) => deleteSellerCoupon(token, id),
    };
  }, [token]);

  if (!ready) {
    return (
      <main>
        <p>লোড হচ্ছে…</p>
      </main>
    );
  }
  if (!token || !api) {
    return (
      <main>
        <h1>কুপন — Coupons</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>আমার স্টোরের কুপন — Store coupons</h1>
      <p>
        স্টোর কুপন শুধু আপনার স্টোরের পণ্যে ছাড় দেয়, আর ছাড়ের
        দায় আপনার — কমিশন হিসাব হবে ছাড়-পরবর্তী দামের উপর।
        (A store coupon discounts only your store's goods, and you
        bear the discount: commission is charged on the discounted
        goods.)
      </p>
      <CouponManager api={api} />
    </main>
  );
}
