"use client";

import { Suspense, useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import PostCard from "@/components/PostCard";
import StoreShopHeader from "@/components/StoreShopHeader";
import {
  listStorePosts,
  storePostFeedUrl,
  type PublicPostList,
} from "@/lib/api";

/**
 * A store's blog index (Wave 59): its live posts,
 * newest first — drafts, scheduled and archived posts
 * never reach this list (the API's live rule). A `tag`
 * query filters to one tag (the post cards' chips link
 * here); the header links the store's Atom feed for
 * feed readers. Pagination is the platform's shared
 * prev/next pattern.
 */
function StoreBlogIndex({ params }: { params: { store: string } }) {
  const searchParams = useSearchParams();
  const tag = searchParams.get("tag") ?? undefined;
  const [data, setData] = useState<PublicPostList | null>(null);
  const [page, setPage] = useState(1);
  const [error, setError] = useState<string | null>(null);
  const LIMIT = 12;

  const reload = useCallback(async () => {
    try {
      const result = await listStorePosts(params.store, {
        tag,
        page,
        limit: LIMIT,
      });
      setData(result);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : "ব্লগ লোড করা যায়নি।");
    }
  }, [params.store, tag, page]);

  useEffect(() => {
    let cancelled = false;
    reload().catch(() => {
      if (!cancelled) setError("ব্লগ লোড করা যায়নি।");
    });
    return () => {
      cancelled = true;
    };
  }, [reload]);

  // A new tag starts again at the first page.
  useEffect(() => {
    setPage(1);
  }, [tag]);

  const pages = data ? Math.max(1, Math.ceil(data.total / LIMIT)) : 1;

  return (
    <main>
      <StoreShopHeader slug={params.store} navCurrent="blog" />
      <h1>ব্লগ — Blog</h1>
      <p>
        <a href={storePostFeedUrl(params.store)} type="application/atom+xml">
          Atom ফিড — Feed (Atom)
        </a>
      </p>
      {tag && (
        <p role="status">
          ট্যাগ: <strong>{tag}</strong>{" "}
          <Link href={`/${params.store}/blog`}>সব পোস্ট দেখুন — All posts</Link>
        </p>
      )}
      {error && <p role="alert">{error}</p>}
      {data === null && !error ? (
        <p>লোড হচ্ছে…</p>
      ) : data && data.items.length === 0 ? (
        <p>
          {tag
            ? "এই ট্যাগে কোনো পোস্ট নেই।"
            : "এখনো কোনো পোস্ট প্রকাশ হয়নি। — No posts published yet."}
        </p>
      ) : data ? (
        <>
          <div className="post-grid">
            {data.items.map((post) => (
              <PostCard key={post.id} post={post} />
            ))}
          </div>
          {pages > 1 && (
            <p>
              <button
                type="button"
                disabled={page <= 1}
                onClick={() => setPage(page - 1)}
              >
                ← আগের
              </button>{" "}
              পৃষ্ঠা {page} / {pages}{" "}
              <button
                type="button"
                disabled={page >= pages}
                onClick={() => setPage(page + 1)}
              >
                পরের →
              </button>
            </p>
          )}
        </>
      ) : null}
    </main>
  );
}

export default function StoreBlogPage({
  params,
}: {
  params: { store: string };
}) {
  return (
    <Suspense fallback={<p>লোড হচ্ছে…</p>}>
      <StoreBlogIndex params={params} />
    </Suspense>
  );
}
