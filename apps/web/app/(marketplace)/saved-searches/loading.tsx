/**
 * Saved-searches route fallback (Wave 53): skeleton rows
 * in the saved-search card shape while the page loads,
 * matching the browse loading treatment so route changes
 * never flash an empty page.
 */
export default function SavedSearchesLoading() {
  return (
    <main aria-busy="true">
      <h1>সংরক্ষিত খোঁজ — Saved searches</h1>
      <div className="saved-search-list">
        {[0, 1, 2].map((row) => (
          <div key={row} className="saved-search-card skeleton-card">
            <div className="skeleton-line skeleton-line-wide" />
            <div className="skeleton-line" />
          </div>
        ))}
      </div>
    </main>
  );
}
