"use client";

import { useEffect, useState } from "react";
import PostBody from "@/components/PostBody";
import StoreShopHeader from "@/components/StoreShopHeader";
import { formatDateTime } from "@/lib/format";
import { getStorePage, type StorePage } from "@/lib/api";

/**
 * One store page (Wave 59): the seller's standing
 * document — about, shipping information, size guide,
 * FAQ — at its own address under the store. The API
 * serves published pages only, so a draft, an archived
 * page and a wrong slug all land on the same honest
 * "not found" here. The body is plain text rendered as
 * paragraphs by PostBody; the store's frame (identity +
 * navigation) is the shared shop header, so the page
 * reads as part of the store, not a stray document.
 */
export default function StorePagePage({
  params,
}: {
  params: { store: string; pageSlug: string };
}) {
  const [page, setPage] = useState<StorePage | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getStorePage(params.store, params.pageSlug)
      .then((data) => {
        if (!cancelled) setPage(data);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [params.store, params.pageSlug]);

  return (
    <main>
      <StoreShopHeader slug={params.store} navCurrent={params.pageSlug} />
      {failed ? (
        <p>
          এই পেজটি পাওয়া যায়নি। (Page not found — it may be unpublished,
          or the address is wrong.)
        </p>
      ) : page === null ? (
        <p>লোড হচ্ছে… (Loading…)</p>
      ) : (
        <article>
          <h1>{page.title}</h1>
          <p className="post-meta">
            সর্বশেষ হালনাগাদ — Updated {formatDateTime(page.updated_at)}
          </p>
          <PostBody body={page.body} />
        </article>
      )}
    </main>
  );
}
