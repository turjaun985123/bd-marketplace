"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import { getSearchAnalytics, type SearchAnalytics } from "@/lib/api";
import { describeFilters } from "@/lib/browse";
import { formatDateTime } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

/**
 * Search analytics (Wave 53): the operator's full view of
 * the identifier-free search log — what shoppers ask for,
 * how much of it the catalogue answers, and what it
 * cannot answer at all (the zero-result list is the
 * stocking list). The window is switchable (7 / 30 / 90
 * days; the API validates 1..=90). Nothing here can be
 * traced to a person: the log has no user id by design,
 * and this page neither needs nor wants one.
 */
const WINDOWS = [7, 30, 90];

export default function AdminSearchAnalyticsPage() {
  return (
    <AdminShell>
      <SearchAnalyticsView />
    </AdminShell>
  );
}

function SearchAnalyticsView() {
  const { token } = useAuth();
  const [days, setDays] = useState(30);
  const [analytics, setAnalytics] = useState<SearchAnalytics | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    setAnalytics(null);
    try {
      setAnalytics(await getSearchAnalytics(token, days));
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "খোঁজের হিসাব লোড করা যায়নি।"
      );
    }
  }, [token, days]);

  useEffect(() => {
    void load();
  }, [load]);

  const zeroSearches =
    analytics?.zero_result_searches.reduce(
      (sum, row) => sum + row.searches,
      0
    ) ?? 0;
  const zeroShare =
    analytics && analytics.total_searches > 0
      ? Math.round((zeroSearches / analytics.total_searches) * 100)
      : 0;

  return (
    <>
      <h2>খোঁজের হিসাব — Search analytics</h2>
      <p className="browse-toolbar-controls">
        সময়সীমা — Window:{" "}
        {WINDOWS.map((window) => (
          <button
            key={window}
            type="button"
            className={days === window ? "is-active" : ""}
            onClick={() => setDays(window)}
          >
            {window} দিন — days
          </button>
        ))}
      </p>
      {message ? (
        <p role="alert">{message}</p>
      ) : !analytics ? (
        <p>লোড হচ্ছে… (Loading…)</p>
      ) : analytics.total_searches === 0 ? (
        <p>
          এই সময়ে কোনো খোঁজ হয়নি। (No searches in this window yet — the
          log fills as shoppers browse.)
        </p>
      ) : (
        <>
          <div className="card">
            <p>
              মোট খোঁজ: <strong>{analytics.total_searches}</strong>টি ·
              ফলাফলহীন খোঁজ: <strong>{zeroSearches}</strong>টি ({zeroShare}%)
            </p>
            <p>
              Total searches: <strong>{analytics.total_searches}</strong> ·
              zero-result searches: <strong>{zeroSearches}</strong> (
              {zeroShare}% of all searches)
            </p>
          </div>

          <h3>সবচেয়ে বেশি খোঁজা — Top searches</h3>
          {analytics.top_searches.length === 0 ? (
            <p>শুধু ফিল্টার-ভিত্তিক ব্রাউজ হয়েছে। (Only filter browses.)</p>
          ) : (
            <table className="merch-table">
              <thead>
                <tr>
                  <th>খোঁজ — Query</th>
                  <th>বার — Searches</th>
                  <th>গড় ফলাফল — Avg results</th>
                </tr>
              </thead>
              <tbody>
                {analytics.top_searches.map((row) => (
                  <tr key={row.query_text}>
                    <td>“{row.query_text}”</td>
                    <td>{row.searches}</td>
                    <td>{row.avg_results}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          <h3>জনপ্রিয় ফিল্টার সমন্বয় — Top filter combinations</h3>
          <p>
            কোন ফিল্টার-সমন্বয়ে ক্রেতারা সবচেয়ে বেশি ব্রাউজ করেন — খোঁজের
            লগের ফিঙ্গারপ্রিন্ট দিয়ে গোষ্ঠীবদ্ধ, ক্যানোনিকাল ফিল্টার আকারে
            পড়া যায়। (The filter combinations shoppers run most, grouped
            by the log's fingerprint and shown in canonical form.)
          </p>
          {analytics.top_filter_combinations.length === 0 ? (
            <p>কোনো সমন্বয় লগ হয়নি। (No combinations logged yet.)</p>
          ) : (
            <table className="merch-table">
              <thead>
                <tr>
                  <th>ফিল্টার — Filters</th>
                  <th>বার — Searches</th>
                  <th>গড় ফলাফল — Avg results</th>
                </tr>
              </thead>
              <tbody>
                {analytics.top_filter_combinations.map((row, index) => (
                  <tr key={index}>
                    <td>{describeFilters(row.filters)}</td>
                    <td>{row.searches}</td>
                    <td>{row.avg_results}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          <h3>কিছুই মেলেনি — Zero-result searches</h3>
          <p>
            এই শব্দগুলো দিয়ে খুঁজে ক্রেতা কিছুই পাননি — ক্যাটালগে এই ঘাটতিই
            পূরণ করার তালিকা। (Shoppers searched these and found nothing —
            this is the catalogue-gap list.)
          </p>
          {analytics.zero_result_searches.length === 0 ? (
            <p>কোনো ফলাফলহীন খোঁজ নেই। (No zero-result searches.)</p>
          ) : (
            <table className="merch-table">
              <thead>
                <tr>
                  <th>খোঁজ — Query</th>
                  <th>বার — Searches</th>
                  <th>শেষ খোঁজ — Last searched</th>
                </tr>
              </thead>
              <tbody>
                {analytics.zero_result_searches.map((row) => (
                  <tr key={row.query_text}>
                    <td>“{row.query_text}”</td>
                    <td>{row.searches}</td>
                    <td>{formatDateTime(row.last_searched_at)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      )}
    </>
  );
}
