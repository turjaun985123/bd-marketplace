"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  createStorePost,
  deleteStorePost,
  deleteStorePostCover,
  getSellerDashboard,
  getSellerPost,
  getSellerPostStats,
  listSellerPosts,
  setStorePostStatus,
  updateStorePost,
  uploadStorePostCover,
  type ContentStatus,
  type PostStats,
  type SellerPost,
} from "@/lib/api";
import { formatDateTime, isoToLocalInput, localInputToIso } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

/**
 * Seller blog (Wave 59): the store's posts — drafts,
 * scheduled, published, archived. The editor writes
 * plain text (paragraphs on blank lines; the storefront
 * renders exactly that), tags as a comma list (the API
 * normalises case and spacing), an optional hand-written
 * excerpt (blank means the blog derives one from the
 * body), and a cover — a hotlink URL or an upload
 * through the media pipeline. Publishing names the
 * moment: a future moment schedules the post, and it
 * goes public on its own when the moment arrives.
 * Below the editor, the stats panel shows the
 * aggregate-only view counts (reads, never readers).
 */
export default function SellerBlogPage() {
  const { ready, token } = useAuth();
  const [storeSlug, setStoreSlug] = useState<string | null>(null);
  const [posts, setPosts] = useState<SellerPost[] | null>(null);
  const [total, setTotal] = useState(0);
  const [stats, setStats] = useState<PostStats | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // List filters.
  const [filterStatus, setFilterStatus] = useState<"" | ContentStatus>("");
  const [search, setSearch] = useState("");
  const [page, setPage] = useState(1);
  const LIMIT = 20;

  // The editor form (create when editingId is null).
  const [editingId, setEditingId] = useState<string | null>(null);
  const [formOpen, setFormOpen] = useState(false);
  const [slug, setSlug] = useState("");
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [excerpt, setExcerpt] = useState("");
  const [tagsText, setTagsText] = useState("");
  const [coverUrl, setCoverUrl] = useState("");
  const [publishAt, setPublishAt] = useState("");
  const [coverFile, setCoverFile] = useState<File | null>(null);

  const reload = useCallback(async () => {
    if (!token) return;
    try {
      const [dashboard, list, postStats] = await Promise.all([
        getSellerDashboard(token),
        listSellerPosts(token, {
          status: filterStatus === "" ? undefined : filterStatus,
          q: search.trim() === "" ? undefined : search.trim(),
          page,
          limit: LIMIT,
        }),
        getSellerPostStats(token),
      ]);
      setStoreSlug(dashboard.store.slug);
      setPosts(list.items);
      setTotal(list.total);
      setStats(postStats);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "ব্লগ লোড করা যায়নি।");
    }
  }, [token, filterStatus, search, page]);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    reload().catch(() => {
      if (!cancelled) setMessage("ব্লগ লোড করা যায়নি।");
    });
    return () => {
      cancelled = true;
    };
  }, [token, reload]);

  function resetForm() {
    setEditingId(null);
    setSlug("");
    setTitle("");
    setBody("");
    setExcerpt("");
    setTagsText("");
    setCoverUrl("");
    setPublishAt("");
    setCoverFile(null);
  }

  function openCreate() {
    resetForm();
    setFormOpen(true);
    setMessage(null);
  }

  async function openEditor(post: SellerPost) {
    if (!token) return;
    setMessage(null);
    try {
      const detail = await getSellerPost(token, post.id);
      setEditingId(detail.id);
      setSlug(detail.slug);
      setTitle(detail.title);
      setBody(detail.body);
      setExcerpt(detail.custom_excerpt ?? "");
      setTagsText(detail.tags.join(", "));
      setCoverUrl(detail.cover_image_url ?? "");
      setPublishAt(isoToLocalInput(detail.published_at));
      setCoverFile(null);
      setFormOpen(true);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "পোস্ট খোলা যায়নি।");
    }
  }

  function parsedTags(): string[] {
    return tagsText
      .split(",")
      .map((tag) => tag.trim())
      .filter((tag) => tag.length > 0);
  }

  async function onSave(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setBusy(true);
    setMessage(null);
    try {
      const input = {
        slug: slug.trim(),
        title: title.trim(),
        body,
        excerpt,
        tags: parsedTags(),
        cover_image_url: coverUrl,
      };
      let postId = editingId;
      if (postId) {
        await updateStorePost(token, postId, input);
      } else {
        const created = await createStorePost(token, input);
        postId = created.id;
      }
      if (coverFile && postId) {
        await uploadStorePostCover(token, postId, coverFile);
      }
      setFormOpen(false);
      resetForm();
      await reload();
      setMessage("পোস্ট সংরক্ষণ হয়েছে। (Post saved.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "সংরক্ষণ করা যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  async function onPublish(post: SellerPost) {
    if (!token) return;
    setBusy(true);
    setMessage(null);
    try {
      const moment =
        editingId === post.id ? localInputToIso(publishAt) : null;
      await setStorePostStatus(
        token,
        post.id,
        "published",
        moment ?? undefined
      );
      await reload();
      setMessage(
        moment && new Date(moment).getTime() > Date.now()
          ? "পোস্ট সময়সূচিতে গেছে — নির্ধারিত সময়ে প্রকাশ হবে। (Post scheduled.)"
          : "পোস্ট প্রকাশ হয়েছে। (Post published.)"
      );
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "প্রকাশ করা যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  async function onStatus(post: SellerPost, status: ContentStatus) {
    if (!token) return;
    setBusy(true);
    setMessage(null);
    try {
      await setStorePostStatus(token, post.id, status);
      await reload();
      setMessage(
        status === "draft"
          ? "পোস্ট খসড়ায় ফিরেছে। (Post is a draft again.)"
          : "পোস্ট আর্কাইভ হয়েছে। (Post archived.)"
      );
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "অবস্থা বদলানো যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  async function onDelete(post: SellerPost) {
    if (!token) return;
    if (
      !window.confirm(
        `"${post.title}" পোস্টটি মুছে ফেলবেন? (Delete the post "${post.title}"?)`
      )
    ) {
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      await deleteStorePost(token, post.id);
      if (editingId === post.id) {
        setFormOpen(false);
        resetForm();
      }
      await reload();
      setMessage("পোস্ট মুছে ফেলা হয়েছে। (Post deleted.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "মুছতে পারা যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  async function onRemoveCover(post: SellerPost) {
    if (!token) return;
    setBusy(true);
    setMessage(null);
    try {
      await deleteStorePostCover(token, post.id);
      if (editingId === post.id) setCoverUrl("");
      await reload();
      setMessage("কভার ছবি সরানো হয়েছে। (Cover removed.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "কভার সরানো যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  const pages = Math.max(1, Math.ceil(total / LIMIT));

  if (!ready) {
    return (
      <main>
        <p>লোড হচ্ছে…</p>
      </main>
    );
  }
  if (!token) {
    return (
      <main>
        <p>
          ব্লগ সম্পাদনা করতে <Link href="/login">সাইন ইন করুন — Sign in</Link>।
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>স্টোর ব্লগ — Store blog</h1>
      <p>
        <Link href="/seller">← ড্যাশবোর্ড — Dashboard</Link>
        {storeSlug && (
          <>
            {" · "}
            <Link href={`/${storeSlug}/blog`}>ব্লগ দেখুন — View blog</Link>
          </>
        )}
      </p>
      {message && <p role="status">{message}</p>}

      <p>
        <button type="button" disabled={busy} onClick={openCreate}>
          নতুন পোস্ট — New post
        </button>
      </p>

      {formOpen && (
        <form onSubmit={(e) => void onSave(e)}>
          <h2>{editingId ? "পোস্ট সম্পাদনা — Edit post" : "নতুন পোস্ট — New post"}</h2>
          <p>
            <label>
              শিরোনাম — Title{" "}
              <input
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                maxLength={160}
                size={60}
                required
              />
            </label>
          </p>
          <p>
            <label>
              স্লাগ — Slug (URL){" "}
              <input
                value={slug}
                onChange={(e) => setSlug(e.target.value)}
                placeholder="eid-guide"
                required
              />
            </label>
          </p>
          <p>
            <label>
              লেখা — Body (সাধারণ টেক্সট; ফাঁকা লাইন = নতুন অনুচ্ছেদ)
              <br />
              <textarea
                value={body}
                onChange={(e) => setBody(e.target.value)}
                rows={14}
                cols={76}
                maxLength={40000}
                required
              />
            </label>
          </p>
          <p>
            <label>
              সারাংশ — Excerpt (খালি রাখলে লেখা থেকে তৈরি হবে){" "}
              <input
                value={excerpt}
                onChange={(e) => setExcerpt(e.target.value)}
                maxLength={300}
                size={60}
              />
            </label>
          </p>
          <p>
            <label>
              ট্যাগ — Tags (কমা দিয়ে আলাদা করুন, সর্বোচ্চ ৮টি){" "}
              <input
                value={tagsText}
                onChange={(e) => setTagsText(e.target.value)}
                placeholder="eid, guide, নতুন"
                size={60}
              />
            </label>
          </p>
          <p>
            <label>
              কভার ছবির URL — Cover image URL{" "}
              <input
                value={coverUrl}
                onChange={(e) => setCoverUrl(e.target.value)}
                placeholder="https://…"
                size={60}
              />
            </label>
          </p>
          <p>
            <label>
              অথবা কভার আপলোড করুন — Or upload a cover{" "}
              <input
                type="file"
                accept="image/*"
                onChange={(e) => setCoverFile(e.target.files?.[0] ?? null)}
              />
            </label>
          </p>
          {editingId && (
            <p>
              <label>
                প্রকাশের সময় — Publish at (ভবিষ্যতের সময় দিলে পোস্ট
                সময়সূচিতে যাবে){" "}
                <input
                  type="datetime-local"
                  value={publishAt}
                  onChange={(e) => setPublishAt(e.target.value)}
                />
              </label>
            </p>
          )}
          <button type="submit" disabled={busy}>
            সংরক্ষণ — Save
          </button>{" "}
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              setFormOpen(false);
              resetForm();
            }}
          >
            বাতিল — Cancel
          </button>
        </form>
      )}

      <h2>সব পোস্ট — All posts</h2>
      <p>
        <label>
          অবস্থা — Status{" "}
          <select
            value={filterStatus}
            onChange={(e) => {
              setFilterStatus(e.target.value as "" | ContentStatus);
              setPage(1);
            }}
          >
            <option value="">সব — All</option>
            <option value="draft">খসড়া — Draft</option>
            <option value="published">প্রকাশিত — Published</option>
            <option value="archived">আর্কাইভ — Archived</option>
          </select>
        </label>{" "}
        <label>
          খুঁজুন — Search{" "}
          <input
            value={search}
            onChange={(e) => {
              setSearch(e.target.value);
              setPage(1);
            }}
            placeholder="শিরোনাম বা স্লাগ"
          />
        </label>
      </p>
      {posts === null ? (
        <p>লোড হচ্ছে…</p>
      ) : posts.length === 0 ? (
        <p>কোনো পোস্ট পাওয়া যায়নি। (No posts found.)</p>
      ) : (
        <>
          <ul className="content-list">
            {posts.map((post) => (
              <li key={post.id}>
                <strong>{post.title}</strong> — /{post.slug} · {post.status}
                {post.status === "published" &&
                  post.published_at &&
                  new Date(post.published_at).getTime() > Date.now() &&
                  " · সময়সূচিতে (scheduled)"}
                <br />
                <small>
                  {formatDateTime(post.published_at)} · {post.view_count} বার
                  পড়া হয়েছে — {post.view_count} reads · আনুমানিক{" "}
                  {post.reading_minutes} মিনিট
                  {post.tags.length > 0 && <> · {post.tags.join(", ")}</>}
                </small>
                <br />
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void openEditor(post)}
                >
                  সম্পাদনা — Edit
                </button>{" "}
                {post.status !== "published" && (
                  <>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => void onPublish(post)}
                    >
                      প্রকাশ করুন — Publish
                    </button>{" "}
                  </>
                )}
                {post.status === "published" && (
                  <>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => void onStatus(post, "draft")}
                    >
                      খসড়া করুন — Unpublish
                    </button>{" "}
                  </>
                )}
                {post.status !== "archived" && (
                  <>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => void onStatus(post, "archived")}
                    >
                      আর্কাইভ — Archive
                    </button>{" "}
                  </>
                )}
                {post.cover_image_url && (
                  <>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => void onRemoveCover(post)}
                    >
                      কভার সরান — Remove cover
                    </button>{" "}
                  </>
                )}
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void onDelete(post)}
                >
                  মুছুন — Delete
                </button>
                {storeSlug && post.status === "published" && (
                  <>
                    {" "}
                    <Link href={`/${storeSlug}/blog/${post.slug}`}>
                      দেখুন — View
                    </Link>
                  </>
                )}
              </li>
            ))}
          </ul>
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
      )}

      <h2>পড়ার পরিসংখ্যান — Read statistics</h2>
      {stats === null ? (
        <p>লোড হচ্ছে…</p>
      ) : (
        <>
          <p>
            মোট {stats.total_views} বার পড়া হয়েছে; গত {stats.window_days}{" "}
            দিনে {stats.window_views} বার। এই সংখ্যা শুধু পড়ার হিসাব —
            কে পড়েছে তা কোথাও রাখা হয় না। (Aggregate reads only — no
            reader is ever recorded.)
          </p>
          {stats.posts.length > 0 && (
            <ul className="content-list">
              {stats.posts.map((line) => (
                <li key={line.post_id}>
                  <strong>{line.title}</strong> — {line.total_views} বার (গত{" "}
                  {stats.window_days} দিনে {line.window_views})
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </main>
  );
}
