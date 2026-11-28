"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  listAdminContentPages,
  listAdminContentPosts,
  setAdminPageStatus,
  setAdminPostStatus,
  type AdminContentPage,
  type AdminContentPost,
  type ContentStatus,
} from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";

/**
 * Admin content moderation (Wave 59): every store's
 * blog posts and standing pages in one queue, with the
 * platform's takedown tools — archive a post or page
 * to pull it from public view, restore it to draft for
 * the seller to fix, or publish it back. Every write
 * goes through the audited admin path (the same state
 * machine the sellers use), so the audit log answers
 * "who took this down" for every item here. Sellers
 * keep their own editors; this page exists because
 * moderation cannot wait for a seller to act.
 */
export default function AdminContentPage() {
  const { ready, token } = useAuth();
  const [posts, setPosts] = useState<AdminContentPost[] | null>(null);
  const [pages, setPages] = useState<AdminContentPage[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [statusFilter, setStatusFilter] = useState<"" | ContentStatus>("");
  const [search, setSearch] = useState("");

  const reload = useCallback(async () => {
    if (!token) return;
    try {
      const opts = {
        status: statusFilter === "" ? undefined : statusFilter,
        q: search.trim() === "" ? undefined : search.trim(),
        limit: 50,
      };
      const [postList, pageList] = await Promise.all([
        listAdminContentPosts(token, opts),
        listAdminContentPages(token, opts),
      ]);
      setPosts(postList.items);
      setPages(pageList.items);
    } catch (err) {
      setError(err instanceof Error ? err.message : "লোড হয়নি — Load failed");
    }
  }, [token, statusFilter, search]);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function actPost(post: AdminContentPost, status: ContentStatus) {
    if (!token) return;
    setBusy(true);
    setError(null);
    try {
      await setAdminPostStatus(token, post.id, status);
      setNotice(
        `"${post.title}" এখন ${status} (অডিট লগে লেখা হয়েছে)। — "${post.title}" is now ${status} (audit-logged).`
      );
      await reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : "বদলানো যায়নি — Failed");
    } finally {
      setBusy(false);
    }
  }

  async function actPage(page: AdminContentPage, status: ContentStatus) {
    if (!token) return;
    setBusy(true);
    setError(null);
    try {
      await setAdminPageStatus(token, page.id, status);
      setNotice(
        `"${page.title}" এখন ${status} (অডিট লগে লেখা হয়েছে)। — "${page.title}" is now ${status} (audit-logged).`
      );
      await reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : "বদলানো যায়নি — Failed");
    } finally {
      setBusy(false);
    }
  }

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
          মডারেশন দেখতে <Link href="/login">সাইন ইন করুন — Sign in</Link>।
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>কনটেন্ট মডারেশন — Content moderation</h1>
      <p>
        <Link href="/admin">← অ্যাডমিন — Admin</Link>
      </p>
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      <p>
        <label>
          অবস্থা — Status{" "}
          <select
            value={statusFilter}
            onChange={(e) => setStatusFilter(e.target.value as "" | ContentStatus)}
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
            onChange={(e) => setSearch(e.target.value)}
            placeholder="শিরোনাম, স্লাগ বা স্টোর"
          />
        </label>
      </p>

      <h2>ব্লগ পোস্ট — Blog posts</h2>
      {posts === null ? (
        <p>লোড হচ্ছে…</p>
      ) : posts.length === 0 ? (
        <p>কোনো পোস্ট নেই। (No posts.)</p>
      ) : (
        <ul className="content-list">
          {posts.map((post) => (
            <li key={post.id}>
              <strong>{post.title}</strong> — {post.status}
              <br />
              <small>
                <Link href={`/${post.store_slug}`}>{post.store_name}</Link> · /
                {post.slug} · {formatDateTime(post.updated_at)}
              </small>
              <br />
              {post.status !== "archived" && (
                <>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void actPost(post, "archived")}
                  >
                    আর্কাইভ (সরিয়ে নিন) — Archive (takedown)
                  </button>{" "}
                </>
              )}
              {post.status !== "draft" && (
                <>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void actPost(post, "draft")}
                  >
                    খসড়া করুন — To draft
                  </button>{" "}
                </>
              )}
              {post.status !== "published" && (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void actPost(post, "published")}
                >
                  প্রকাশ করুন — Publish
                </button>
              )}{" "}
              <Link href={`/${post.store_slug}/blog/${post.slug}`}>
                দেখুন — View
              </Link>
            </li>
          ))}
        </ul>
      )}

      <h2>স্টোর পেজ — Store pages</h2>
      {pages === null ? (
        <p>লোড হচ্ছে…</p>
      ) : pages.length === 0 ? (
        <p>কোনো পেজ নেই। (No pages.)</p>
      ) : (
        <ul className="content-list">
          {pages.map((page) => (
            <li key={page.id}>
              <strong>{page.title}</strong> — {page.status}
              {page.show_in_nav && " · নেভিগেশনে (in nav)"}
              <br />
              <small>
                <Link href={`/${page.store_slug}`}>{page.store_name}</Link> · /
                {page.slug} · {formatDateTime(page.updated_at)}
              </small>
              <br />
              {page.status !== "archived" && (
                <>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void actPage(page, "archived")}
                  >
                    আর্কাইভ (সরিয়ে নিন) — Archive (takedown)
                  </button>{" "}
                </>
              )}
              {page.status !== "draft" && (
                <>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void actPage(page, "draft")}
                  >
                    খসড়া করুন — To draft
                  </button>{" "}
                </>
              )}
              {page.status !== "published" && (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void actPage(page, "published")}
                >
                  প্রকাশ করুন — Publish
                </button>
              )}{" "}
              <Link href={`/${page.store_slug}/pages/${page.slug}`}>
                দেখুন — View
              </Link>
            </li>
          ))}
        </ul>
      )}
    </main>
  );
}
