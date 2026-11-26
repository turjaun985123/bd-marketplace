"use client";

import Link from "next/link";
import { useAuth } from "@/hooks/useAuth";
import { useWishlist } from "@/hooks/useWishlist";

/**
 * The heart toggle (Wave 24) — one button, used by the
 * product card and the product page. State comes from the
 * shared WishlistProvider (one ids fetch per session, never
 * one per card). Signed-out visitors get a login link in
 * the same shape instead: saving is an account feature.
 */
export default function WishlistButton({
  productId,
}: {
  productId: string;
}) {
  const { token } = useAuth();
  const { isWishlisted, toggle } = useWishlist();

  if (!token) {
    return (
      <Link href="/login" title="উইশলিস্টে রাখতে লগইন করুন">
        ♡ উইশলিস্ট
      </Link>
    );
  }

  const saved = isWishlisted(productId);
  return (
    <button
      type="button"
      aria-pressed={saved}
      title={saved ? "উইশলিস্ট থেকে সরান" : "উইশলিস্টে রাখুন"}
      onClick={() => void toggle(productId)}
    >
      {saved ? "♥ উইশলিস্টে আছে" : "♡ উইশলিস্ট"}
    </button>
  );
}
