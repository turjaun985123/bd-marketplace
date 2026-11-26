/**
 * DailyBars (Wave 27): the analytics pages' one chart — a row
 * of CSS-height bars, one per day, heights relative to the
 * series maximum. No chart library: each bar is a div whose
 * title carries the exact figures, and the values also live
 * in the tables/cards around it, so nothing depends on
 * reading bar heights precisely.
 */
export interface DailyBarRow {
  date: string;
  value: number;
  /** Full hover text, e.g. "2026-11-26 — ৳2,500.00 · 2টি অর্ডার". */
  title: string;
}

export default function DailyBars({ rows }: { rows: DailyBarRow[] }) {
  const max = Math.max(0, ...rows.map((row) => row.value));
  return (
    <div className="daily-bars" role="img" aria-label="দৈনিক বার চার্ট">
      {rows.map((row) => (
        <div
          key={row.date}
          className="daily-bar"
          title={row.title}
          style={{
            height: `${max > 0 ? Math.max(2, Math.round((row.value / max) * 100)) : 2}%`,
          }}
        />
      ))}
    </div>
  );
}
