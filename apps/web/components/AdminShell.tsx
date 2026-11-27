"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { getMe, type User } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The admin area's shared shell + guard (Wave 10). Every admin
 * page renders inside this component: it refreshes the signed-in
 * user from GET /v1/auth/me (the login snapshot may be stale) and
 * only renders the page for admins — everyone else sees
 * "অ্যাক্সেস নেই". The API enforces the same rule server-side
 * (403); this guard is the friendly face of it, not the security.
 */
export default function AdminShell({
  children,
}: {
  children: React.ReactNode;
}) {
  const { ready, token } = useAuth();
  const [me, setMe] = useState<User | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getMe(token)
      .then((user) => {
        if (!cancelled) setMe(user);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  if (!ready) {
    return (
      <main>
        <p>লোড হচ্ছে…</p>
      </main>
    );
  }
  if (!token) {
    return (
      <main>
        <h1>অ্যাডমিন প্যানেল</h1>
        <p>
          অ্যাডমিন প্যানেল দেখতে আগে <Link href="/login">লগইন</Link> করুন।
        </p>
      </main>
    );
  }
  if (failed) {
    return (
      <main>
        <h1>অ্যাডমিন প্যানেল</h1>
        <p>ব্যবহারকারীর তথ্য লোড করা যায়নি — API চলছে তো?</p>
      </main>
    );
  }
  if (!me) {
    return (
      <main>
        <p>লোড হচ্ছে…</p>
      </main>
    );
  }
  if (!me.is_admin) {
    return (
      <main>
        <h1>অ্যাক্সেস নেই</h1>
        <p>
          এই পেজটি শুধু প্ল্যাটফর্ম অ্যাডমিনদের জন্য। আপনার অ্যাকাউন্টে
          অ্যাডমিন অ্যাক্সেস নেই।
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>অ্যাডমিন প্যানেল</h1>
      <p>
        <Link href="/admin">ওভারভিউ</Link>
        {" · "}
        <Link href="/admin/analytics">অ্যানালিটিক্স</Link>
        {" · "}
        <Link href="/admin/search-analytics">খোঁজের হিসাব</Link>
        {" · "}
        <Link href="/admin/stores">স্টোর</Link>
        {" · "}
        <Link href="/admin/payouts">পে-আউট</Link>
        {" · "}
        <Link href="/admin/coupons">কুপন</Link>
        {" · "}
        <Link href="/admin/banners">ব্যানার</Link>
        {" · "}
        <Link href="/admin/collections">কালেকশন</Link>
        {" · "}
        <Link href="/admin/categories">ক্যাটাগরি</Link>
        {" · "}
        <Link href="/admin/categories/tree">ক্যাটাগরি ট্রি</Link>
        {" · "}
        <Link href="/admin/flash-sales">ফ্ল্যাশ সেল</Link>
        {" · "}
        <Link href="/admin/home-preview">হোম প্রিভিউ</Link>
        {" · "}
        <Link href="/admin/returns">রিটার্ন ও রিফান্ড</Link>
        {" · "}
        <Link href="/admin/verifications">যাচাইকরণ</Link>
        {" · "}
        <Link href="/admin/users">ব্যবহারকারী</Link>
        {" · "}
        <Link href="/admin/settings">সেটিংস</Link>
        {" · "}
        <Link href="/admin/audit">অডিট লগ</Link>
        {" · "}
        <Link href="/admin/notifications">নোটিফিকেশন</Link>
        {" · "}
        <Link href="/">মার্কেটপ্লেস</Link>
      </p>
      {children}
    </main>
  );
}
