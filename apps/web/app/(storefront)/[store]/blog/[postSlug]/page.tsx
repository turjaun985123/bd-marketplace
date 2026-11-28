"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import PostBody from "@/components/PostBody";
import PostCard from "@/components/PostCard";
import StoreShopHeader from "@/components/StoreShopHeader";
import { formatDateTime } from "@/lib/format";
import {
  getStorePost,
  recordStorePostView,
  type PublicPostDetail,
} from "@/lib/api";

/**
 * One blog post (Wave 59): the full plain-text body as
 * paragraphs, the tag chips (linking to the store's
 * blog filtered to that tag), the previous/next
 * navigation in the blog's publish order, and the
 * related rail — posts ranked by shared tags, present
 * only when a real relation exists. The read fires the
 * aggregate-only view beacon once per mount (no user,
 * no session — the seller's stats count reads, never
 * readers). The BlogPosting JSON-LD is built from the
 * same loaded post. A draft, scheduled or archived
 * post answers the API's uniform 404 and renders the
 * same not-found as a wrong slug.
 */
export default function StoreBlogPostPage({
  params,
}: {
  params: { store: string; postSlug: string };
}) {
  const [post, setPost] = useState<PublicPostDetail | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getStorePost(params.store, params.postSlug)
      .then((data) => {
        if (!cancelled) setPost(data);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    // The view beacon: one read, counted once, failures
    // ignored by design (a blocked beacon must never
    // break the reading).
    recordStorePostView(params.store, params.postSlug).catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [params.store, params.postSlug]);

  return (
    <main>
      <StoreShopHeader slug={params.store} navCurrent="blog" />
      <p>
        <Link href={`/${params.store}/blog`}>← ব্লগে ফিরুন — Back to the blog</Link>
      </p>
      {failed ? (
        <p>
          এই পোস্টটি পাওয়া যায়নি। (Post not found — it may be unpublished,
          scheduled for later, or the address is wrong.)
        </p>
      ) : post === null ? (
        <p>লোড হচ্ছে… (Loading…)</p>
      ) : (
        <article>
          <script
            type="application/ld+json"
            dangerouslySetInnerHTML={{
              __html: JSON.stringify({
                "@context": "https://schema.org",
                "@type": "BlogPosting",
                headline: post.title,
                description: post.excerpt,
                image: post.cover_image_url ?? undefined,
                datePublished: post.published_at ?? undefined,
                author: {
                  "@type": "Organization",
                  name: post.store_name,
                },
                keywords: post.tags.length > 0 ? post.tags.join(", ") : undefined,
              }),
            }}
          />
          <h1>{post.title}</h1>
          <p className="post-meta">
            {formatDateTime(post.published_at)} · আনুমানিক {post.reading_minutes}{" "}
            মিনিট পড়া — {post.reading_minutes} min read
          </p>
          {post.cover_image_url && (
            // eslint-disable-next-line @next/next/no-img-element
            <img className="post-cover" src={post.cover_image_url} alt="" />
          )}
          <PostBody body={post.body} />
          {post.tags.length > 0 && (
            <p className="post-card-tags">
              {post.tags.map((tag) => (
                <Link
                  key={tag}
                  className="tag-chip"
                  href={`/${params.store}/blog?tag=${encodeURIComponent(tag)}`}
                >
                  {tag}
                </Link>
              ))}
            </p>
          )}
          {(post.previous || post.next) && (
            <nav className="post-nav" aria-label="Posts">
              {post.previous && (
                <p>
                  ← আগের পোস্ট — Previous:{" "}
                  <Link href={`/${params.store}/blog/${post.previous.slug}`}>
                    {post.previous.title}
                  </Link>
                </p>
              )}
              {post.next && (
                <p>
                  পরের পোস্ট — Next:{" "}
                  <Link href={`/${params.store}/blog/${post.next.slug}`}>
                    {post.next.title}
                  </Link>{" "}
                  →
                </p>
              )}
            </nav>
          )}
          {post.related.length > 0 && (
            <section>
              <h2>সম্পর্কিত পোস্ট — Related posts</h2>
              <div className="post-grid">
                {post.related.map((related) => (
                  <PostCard key={related.id} post={related} />
                ))}
              </div>
            </section>
          )}
        </article>
      )}
    </main>
  );
}
