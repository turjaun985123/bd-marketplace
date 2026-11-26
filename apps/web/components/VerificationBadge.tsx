import type { VerificationTier } from "@/lib/api";

/**
 * The store verification badge (Wave 21): shown next to a
 * store's name wherever buyers see it (storefront header,
 * product page). `basic` means the platform approved the
 * seller's NID; `verified` means NID plus a trade license.
 * An unverified store renders NOTHING — the absence of a
 * badge is the signal, so there is no "unverified" label to
 * shame new sellers with.
 */
export default function VerificationBadge({
  tier,
}: {
  tier: VerificationTier;
}) {
  if (tier === "basic") {
    return (
      <span className="verification-badge verification-badge-basic">
        যাচাইকৃত বিক্রেতা ✓
      </span>
    );
  }
  if (tier === "verified") {
    return (
      <span className="verification-badge verification-badge-verified">
        যাচাইকৃত ব্যবসা ✓✓
      </span>
    );
  }
  return null;
}
