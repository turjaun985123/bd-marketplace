"use client";

import { useMemo } from "react";
import AdminShell from "@/components/AdminShell";
import CouponManager, { type CouponManagerApi } from "@/components/CouponManager";
import {
  createAdminCoupon,
  listAdminCoupons,
  setAdminCouponActive,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Admin coupons (Wave 14): platform coupons, created and managed
 * inside the admin shell. A platform coupon discounts the whole
 * order's goods and the PLATFORM bears the discount — seller
 * commissions stay on the full goods (see the README's Wave 14
 * money design). The list shows every coupon on the platform
 * (store coupons included, marked with their store); toggling
 * works on any of them. The form and list are the shared
 * CouponManager; this page only wires the admin endpoints in.
 */
export default function AdminCouponsPage() {
  return (
    <AdminShell>
      <Coupons />
    </AdminShell>
  );
}

function Coupons() {
  const { token } = useAuth();

  const api = useMemo<CouponManagerApi | null>(() => {
    if (!token) return null;
    return {
      load: async () => (await listAdminCoupons(token, { limit: 50 })).items,
      create: (input) => createAdminCoupon(token, input),
      setActive: (id, active) => setAdminCouponActive(token, id, active),
    };
  }, [token]);

  if (!api) {
    return <p>লোড হচ্ছে…</p>;
  }

  return (
    <>
      <h2>প্ল্যাটফর্ম কুপন — Platform coupons</h2>
      <p>
        প্ল্যাটফর্ম কুপন পুরো অর্ডারের পণ্যে ছাড় দেয়, আর ছাড়ের
        দায় প্ল্যাটফর্মের — বিক্রেতার কমিশন পুরো দামের উপরই
        থাকবে। (A platform coupon discounts the whole order's
        goods and the platform bears it: seller commissions stay
        on the full goods.)
      </p>
      <CouponManager api={api} />
    </>
  );
}
