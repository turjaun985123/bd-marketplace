"use client";

import Link from "next/link";
import { useAuth } from "@/hooks/useAuth";

/**
 * The "Admin" nav link (Wave 10): rendered only when the stored
 * signed-in user is an admin. The stored user is the login
 * snapshot — the admin area itself re-checks via /v1/auth/me.
 */
export default function AdminLink() {
  const { user } = useAuth();
  if (!user?.is_admin) return null;
  return (
    <>
      {" · "}
      <Link href="/admin">অ্যাডমিন — Admin</Link>
    </>
  );
}
