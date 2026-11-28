import Link from "next/link";
import { formatDateTime } from "@/lib/format";
import type { PublicPost } from "@/lib/api";

/**
 * One blog post card (Wave 59): cover, title, excerpt,
 * and the honest meta line — publish date and the
 * reading-time estimate. Used by the store blog index,
 * the related rail on the post page, and the platform
 * blog discovery page; `showStore` adds the store line
 * the cross-store surfaces need. The excerpt is the
 * API's excerpt in effect (the seller's own or the
 * body-derived one) — the card never invents teaser
 * text of its own.
 */
export default function PostCard({
  post,
  showStore = false,
}: {
  post: PublicPost;
  showStore?: boolean;
}) {
  const href = `/${post.store_slug}/blog/${post.slug}`;
  return (
    <article className="post-card">
      {post.cover_image_url && (
        <Link href={href}>
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img className="post-card-cover" src={post.cover_image_url} alt="" />
        </Link>
      )}
      <h3>
        <Link href={href}>{post.title}</Link>
      </h3>
      {showStore && (
        <p className="post-card-store">
          <Link href={`/${post.store_slug}`}>{post.store_name}</Link>
        </p>
      )}
      <p>{post.excerpt}</p>
      <p className="post-card-meta">
        {formatDateTime(post.published_at)} · আনুমানিক {post.reading_minutes}{" "}
        মিনিট পড়া — {post.reading_minutes} min read
      </p>
      {post.tags.length > 0 && (
        <p className="post-card-tags">
          {post.tags.map((tag) => (
            <Link
              key={tag}
              className="tag-chip"
              href={`/${post.store_slug}/blog?tag=${encodeURIComponent(tag)}`}
            >
              {tag}
            </Link>
          ))}
        </p>
      )}
    </article>
  );
}
