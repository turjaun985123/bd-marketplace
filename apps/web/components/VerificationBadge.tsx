import type { VerificationTier } from "@/lib/api";

/**
 * The store verification badge (Wave 21): shown next to a
 * store's name wherever buyers see it (storefront header,
 * product page). `basic` means the platform approved the
 * seller's NID; `verified` means NID plus a trade license.
 * An unverified store renders NOTHING — the absence of a
 * badge is the signal, so there is no "unverified" label to
 * shame new sellers with.
 *
 * The `title` (admin verifications remake) states the
 * mechanism honestly wherever a hover is available: the
 * platform team reviewed the seller's documents BY HAND —
 * no government database check exists. It is a `title`
 * attribute only (the codebase's convention); touch and
 * print readers get the badge text alone, which makes no
 * mechanism claim either way.
 */
const MANUAL_REVIEW_TITLE =
  "প্ল্যাটফর্ম দল ডকুমেন্ট দেখে হাতে যাচাই করেছে — Platform team manually reviewed the seller's documents";

export default function VerificationBadge({
  tier,
}: {
  tier: VerificationTier;
}) {
  if (tier === "basic") {
    return (
      <span className="verification-badge verification-badge-basic" title={MANUAL_REVIEW_TITLE}>
        যাচাইকৃত বিক্রেতা ✓
      </span>
    );
  }
  if (tier === "verified") {
    return (
      <span className="verification-badge verification-badge-verified" title={MANUAL_REVIEW_TITLE}>
        যাচাইকৃত ব্যবসা ✓✓
      </span>
    );
  }
  return null;
}
