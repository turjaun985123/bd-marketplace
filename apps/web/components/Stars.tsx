/**
 * Shared star-rating display (Wave 8) — built once, reused by the
 * product card, the product detail page and review lists. Renders
 * five stars with the filled portion clipped to the average, so a
 * 3.5 average shows three and a half filled stars. Display only;
 * rating input lives on the product detail page's review form.
 */
export default function Stars({ value }: { value: number | null }) {
  if (value === null) {
    return <span aria-label="কোনো রেটিং নেই">☆☆☆☆☆</span>;
  }
  const pct = Math.max(0, Math.min(100, (value / 5) * 100));
  return (
    <span
      className="stars"
      role="img"
      aria-label={`রেটিং ${value.toFixed(1)} / 5`}
      style={{ position: "relative", display: "inline-block" }}
    >
      <span style={{ color: "#bbb" }}>★★★★★</span>
      <span
        style={{
          position: "absolute",
          left: 0,
          top: 0,
          width: `${pct}%`,
          overflow: "hidden",
          color: "#e8a317",
          whiteSpace: "nowrap",
        }}
      >
        ★★★★★
      </span>
    </span>
  );
}
