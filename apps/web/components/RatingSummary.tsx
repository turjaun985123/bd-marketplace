"use client";

import { useEffect, useState } from "react";
import Stars from "./Stars";
import {
  getProductRatingSummary,
  getStoreRatingSummary,
  type RatingSummary as Summary,
} from "@/lib/api";

/**
 * The rating summary block (Wave 54): the average and
 * total beside the five distribution bars — one row per
 * star level with its count and its floored percent
 * share (the API computes the shares; the bar widths use
 * the same percents, so the picture and the numbers are
 * the same fact). Serves both the product page (per
 * product) and the store reviews page (per store) — one
 * component, two reads of the same API shape. An
 * unreviewed subject renders the honest empty line, and
 * a failed read renders nothing (the review list below
 * still carries the facts).
 */
export default function RatingSummary({
  productId,
  storeSlug,
}: {
  productId?: string;
  storeSlug?: string;
}) {
  const [summary, setSummary] = useState<Summary | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const read = productId
      ? getProductRatingSummary(productId)
      : getStoreRatingSummary(storeSlug ?? "");
    read
      .then((data) => {
        if (!cancelled) setSummary(data);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [productId, storeSlug]);

  if (failed) return null;
  if (summary === null) return <p>রেটিং লোড হচ্ছে…</p>;
  if (summary.rating_count === 0) {
    return <p>এখনো কোনো রিভিউ নেই — No reviews yet.</p>;
  }

  return (
    <div className="rating-summary">
      <div className="rating-summary-head">
        <span className="rating-summary-avg">
          {summary.rating_avg?.toFixed(1)}
        </span>
        <span>
          <Stars value={summary.rating_avg} />
          <br />
          {summary.rating_count}টি রিভিউ
          {summary.with_photos_count > 0 &&
            ` · ${summary.with_photos_count}টি ছবিসহ`}
        </span>
      </div>
      <div className="rating-summary-bars">
        {summary.distribution.map((level) => (
          <div key={level.stars} className="rating-summary-row">
            <span className="rating-summary-stars">{level.stars} ★</span>
            <span className="rating-summary-track">
              <span
                className="rating-summary-fill"
                style={{ width: `${level.percent}%` }}
              />
            </span>
            <span className="rating-summary-count">
              {level.count} ({level.percent}%)
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
