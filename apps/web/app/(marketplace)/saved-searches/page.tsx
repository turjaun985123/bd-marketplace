"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { Button, EmptyState } from "@/components/ui";
import {
  deleteSavedSearch,
  listSavedSearches,
  type SavedSearch,
} from "@/lib/api";
import {
  browseStateFromSaved,
  browseUrl,
  describeFilters,
} from "@/lib/browse";
import { formatDateTime } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

/**
 * সংরক্ষিত খোঁজ — Saved searches (Wave 53): the signed-in
 * buyer's stored browse states. Each row summarises its
 * filters in words, applies them to /browse in one click
 * (the state travels in the URL, like every browse), and
 * deletes on request. Login-gated: a signed-out visitor
 * gets the explanation and the login link, never an error.
 */
export default function SavedSearchesPage() {
  const { ready, token } = useAuth();
  const [searches, setSearches] = useState<SavedSearch[] | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listSavedSearches(token);
      setSearches(list.items);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "সংরক্ষিত খোঁজ লোড করা যায়নি।"
      );
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onDelete(id: string) {
    if (!token) return;
    setMessage(null);
    try {
      await deleteSavedSearch(token, id);
      setSearches((current) =>
        current ? current.filter((entry) => entry.id !== id) : current
      );
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "মুছে ফেলা যায়নি।");
    }
  }

  return (
    <main>
      <p>
        <Link href="/browse">← ব্রাউজ — Browse</Link>
      </p>
      <h1>সংরক্ষিত খোঁজ — Saved searches</h1>

      {!ready ? (
        <p>লোড হচ্ছে… (Loading…)</p>
      ) : !token ? (
        <EmptyState title="লগইন করুন — Sign in">
          <p>
            সংরক্ষিত খোঁজ আপনার অ্যাকাউন্টের সঙ্গে থাকে — যেকোনো ডিভাইসে
            লগইন করলেই পাবেন। (Saved searches live with your account.)
          </p>
          <p>
            <Link href="/login">লগইন — Log in</Link>
          </p>
        </EmptyState>
      ) : message ? (
        <p role="alert">{message}</p>
      ) : searches === null ? (
        <p>লোড হচ্ছে… (Loading…)</p>
      ) : searches.length === 0 ? (
        <EmptyState title="কোনো সংরক্ষিত খোঁজ নেই — No saved searches yet">
          <p>
            ব্রাউজ পেজে ফিল্টার সাজিয়ে “খোঁজটি সংরক্ষণ করুন” চাপলেই এখানে
            জমা হবে। (Set up filters on the browse page and save the search —
            it will appear here.)
          </p>
          <p>
            <Link href="/browse">ব্রাউজ করুন — Browse</Link>
          </p>
        </EmptyState>
      ) : (
        <div className="saved-search-list">
          {searches.map((saved) => (
            <article key={saved.id} className="saved-search card">
              <h2 className="saved-search-name">{saved.name}</h2>
              <p className="saved-search-summary">
                {describeFilters(saved.filters)}
              </p>
              <p className="saved-search-date">
                সংরক্ষিত: {formatDateTime(saved.created_at)}
              </p>
              <p className="saved-search-actions">
                <Link
                  className="saved-search-apply"
                  href={browseUrl(
                    "/browse",
                    browseStateFromSaved(saved.filters)
                  )}
                >
                  এই খোঁজ চালান — Run this search
                </Link>{" "}
                <Button
                  type="button"
                  size="sm"
                  variant="danger"
                  onClick={() => void onDelete(saved.id)}
                >
                  মুছুন — Delete
                </Button>
              </p>
            </article>
          ))}
        </div>
      )}
    </main>
  );
}
