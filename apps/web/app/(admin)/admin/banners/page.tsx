"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import { Badge, Button, EmptyState } from "@/components/ui";
import { useAuth } from "@/hooks/useAuth";
import {
  createAdminBanner,
  deleteAdminBanner,
  deleteAdminBannerImage,
  listAdminBanners,
  updateAdminBanner,
  uploadAdminBannerImage,
  type BannerInput,
  type HomeBanner,
} from "@/lib/api";
import {
  formatDateTime,
  isoToLocalInput,
  localInputToIso,
} from "@/lib/format";

/**
 * Where a banner's window stands right now, by the same
 * half-open rule the API's feed applies (visible from
 * `starts_at` up to but not including `ends_at`; a null
 * bound is unbounded). A display aid for the admin list —
 * the feed remains the authority.
 */
function windowState(banner: HomeBanner): "live" | "scheduled" | "ended" {
  const now = Date.now();
  if (banner.starts_at && new Date(banner.starts_at).getTime() > now) {
    return "scheduled";
  }
  if (banner.ends_at && new Date(banner.ends_at).getTime() <= now) {
    return "ended";
  }
  return "live";
}

/**
 * Home banners (Wave 52): the admin surface for the home
 * page hero carousel. A banner carries its copy, its link,
 * ONE image source (an upload through the media pipeline or
 * a hotlink — setting one replaces the other, exactly like
 * product images), its carousel position, and an optional
 * visibility window; the home feed shows only banners whose
 * window covers "now", in position order.
 *
 * The form edits one banner at a time (create or update);
 * the image controls live on each list row because an
 * upload needs the saved banner's id first.
 */
export default function AdminBannersPage() {
  const { token } = useAuth();
  const [banners, setBanners] = useState<HomeBanner[]>([]);
  const [page, setPage] = useState(1);
  const [total, setTotal] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const PAGE_SIZE = 20;

  // The editor form state. `editingId === null` means the
  // form creates; otherwise it patches that banner.
  const [editingId, setEditingId] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [subtitle, setSubtitle] = useState("");
  const [linkUrl, setLinkUrl] = useState("");
  const [externalImageUrl, setExternalImageUrl] = useState("");
  const [position, setPosition] = useState("0");
  const [startsAt, setStartsAt] = useState("");
  const [endsAt, setEndsAt] = useState("");
  const [isActive, setIsActive] = useState(true);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const data = await listAdminBanners(token, { page, limit: PAGE_SIZE });
      setBanners(data.items);
      setTotal(data.total);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [token, page]);

  useEffect(() => {
    void load();
  }, [load]);

  function resetForm() {
    setEditingId(null);
    setTitle("");
    setSubtitle("");
    setLinkUrl("");
    setExternalImageUrl("");
    setPosition("0");
    setStartsAt("");
    setEndsAt("");
    setIsActive(true);
  }

  function startEdit(banner: HomeBanner) {
    setEditingId(banner.id);
    setTitle(banner.title);
    setSubtitle(banner.subtitle ?? "");
    setLinkUrl(banner.link_url ?? "");
    // The stored hotlink is not echoed by the API (only the
    // resolved display URL), so the field starts blank: a
    // value typed here REPLACES the image source; leaving it
    // blank keeps the current image untouched.
    setExternalImageUrl("");
    setPosition(String(banner.position));
    setStartsAt(isoToLocalInput(banner.starts_at));
    setEndsAt(isoToLocalInput(banner.ends_at));
    setIsActive(banner.is_active);
    setError(null);
  }

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!token) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const input: BannerInput = {
        title: title.trim(),
        subtitle: subtitle.trim() === "" ? null : subtitle.trim(),
        link_url: linkUrl.trim() === "" ? null : linkUrl.trim(),
        position: Number.parseInt(position, 10) || 0,
        starts_at: localInputToIso(startsAt),
        ends_at: localInputToIso(endsAt),
        is_active: isActive,
      };
      if (externalImageUrl.trim() !== "") {
        input.external_image_url = externalImageUrl.trim();
      }
      if (editingId) {
        await updateAdminBanner(token, editingId, input);
        setNotice("ব্যানার আপডেট হয়েছে — Banner updated.");
      } else {
        await createAdminBanner(token, { ...input, title: title.trim() });
        setNotice("ব্যানার তৈরি হয়েছে — Banner created.");
      }
      resetForm();
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  async function onDelete(banner: HomeBanner) {
    if (!token) return;
    if (!window.confirm(`"${banner.title}" ব্যানারটি মুছবেন? (Delete this banner?)`)) {
      return;
    }
    setError(null);
    try {
      await deleteAdminBanner(token, banner.id);
      if (editingId === banner.id) resetForm();
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function onUpload(banner: HomeBanner, file: File | null) {
    if (!token || !file) return;
    setError(null);
    setNotice(null);
    try {
      await uploadAdminBannerImage(token, banner.id, file);
      setNotice("ছবি আপলোড হয়েছে — Banner image uploaded.");
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function onDeleteImage(banner: HomeBanner) {
    if (!token) return;
    setError(null);
    try {
      await deleteAdminBannerImage(token, banner.id);
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  return (
    <AdminShell>
      <h1>হোম ব্যানার — Home banners</h1>
      <p>
        হোমপেজের হিরো ক্যারোসেল। শুধু সক্রিয় ব্যানার, যার উইন্ডো বর্তমান
        সময়কে ধরে, সেগুলোই দেখায় — পজিশন অনুযায়ী। (The home page hero
        carousel. Only active banners whose window covers now appear, in
        position order.)
      </p>
      {error && (
        <p role="alert" className="merch-error">
          {error}
        </p>
      )}
      {notice && <p>{notice}</p>}

      <form onSubmit={onSubmit} className="merch-form">
        <h2>
          {editingId
            ? "ব্যানার সম্পাদনা — Edit banner"
            : "নতুন ব্যানার — New banner"}
        </h2>
        <label>
          শিরোনাম — Title (required)
          <input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            required
            maxLength={120}
          />
        </label>
        <label>
          সাবটাইটেল — Subtitle
          <input
            value={subtitle}
            onChange={(e) => setSubtitle(e.target.value)}
            maxLength={200}
          />
        </label>
        <label>
          লিংক — Link URL (site path like /search?category=… or https://…)
          <input
            value={linkUrl}
            onChange={(e) => setLinkUrl(e.target.value)}
            maxLength={500}
            placeholder="/collections/eid-special"
          />
        </label>
        <label>
          বাইরের ছবির URL — External image URL (সেট করলে আপলোড করা ছবি বদলে
          যাবে — setting this replaces an uploaded image)
          <input
            value={externalImageUrl}
            onChange={(e) => setExternalImageUrl(e.target.value)}
            placeholder="https://…"
          />
        </label>
        <div className="merch-form-row">
          <label>
            পজিশন — Position
            <input
              type="number"
              value={position}
              onChange={(e) => setPosition(e.target.value)}
            />
          </label>
          <label>
            শুরু — Starts at (খালি = সবসময় — empty = always)
            <input
              type="datetime-local"
              value={startsAt}
              onChange={(e) => setStartsAt(e.target.value)}
            />
          </label>
          <label>
            শেষ — Ends at
            <input
              type="datetime-local"
              value={endsAt}
              onChange={(e) => setEndsAt(e.target.value)}
            />
          </label>
          <label>
            <input
              type="checkbox"
              checked={isActive}
              onChange={(e) => setIsActive(e.target.checked)}
            />{" "}
            সক্রিয় — Active
          </label>
        </div>
        <p>
          <Button type="submit" variant="primary" loading={busy}>
            {editingId ? "আপডেট করুন — Update" : "তৈরি করুন — Create"}
          </Button>{" "}
          {editingId && (
            <Button variant="ghost" onClick={resetForm}>
              বাতিল — Cancel edit
            </Button>
          )}
        </p>
      </form>

      {banners.length === 0 ? (
        <EmptyState title="কোনো ব্যানার নেই — No banners yet">
          <p>প্রথম ব্যানার তৈরি করলেই হিরো ক্যারোসেল চালু হবে।</p>
        </EmptyState>
      ) : (
        <table className="merch-table">
          <thead>
            <tr>
              <th>ছবি — Image</th>
              <th>শিরোনাম — Title</th>
              <th>উইন্ডো — Window</th>
              <th>অবস্থা — State</th>
              <th>পদক্ষেপ — Actions</th>
            </tr>
          </thead>
          <tbody>
            {banners.map((banner) => (
              <tr key={banner.id}>
                <td>
                  {banner.image_url ? (
                    // eslint-disable-next-line @next/next/no-img-element
                    <img
                      src={banner.image_url}
                      alt=""
                      className="merch-thumb"
                    />
                  ) : (
                    "—"
                  )}
                </td>
                <td>
                  <strong>{banner.title}</strong>
                  {banner.subtitle ? <div>{banner.subtitle}</div> : null}
                  {banner.link_url ? <div>{banner.link_url}</div> : null}
                  <div>পজিশন — position {banner.position}</div>
                </td>
                <td>
                  {formatDateTime(banner.starts_at)} →{" "}
                  {formatDateTime(banner.ends_at)}
                </td>
                <td>
                  {banner.is_active ? (
                    <Badge tone="brand">সক্রিয় — Active</Badge>
                  ) : (
                    <Badge tone="muted">বন্ধ — Off</Badge>
                  )}{" "}
                  {banner.is_active && (
                    <Badge tone={windowState(banner) === "live" ? "flash" : "muted"}>
                      {windowState(banner) === "live"
                        ? "এখন লাইভ — Live now"
                        : windowState(banner) === "scheduled"
                          ? "সময়সূচিতে — Scheduled"
                          : "শেষ — Ended"}
                    </Badge>
                  )}
                </td>
                <td>
                  <div className="merch-actions">
                    <Button size="sm" variant="ghost" onClick={() => startEdit(banner)}>
                      সম্পাদনা — Edit
                    </Button>
                    <label className="btn btn--ghost btn--sm">
                      ছবি আপলোড — Upload image
                      <input
                        type="file"
                        accept="image/*"
                        hidden
                        onChange={(e) => {
                          void onUpload(banner, e.target.files?.[0] ?? null);
                          e.target.value = "";
                        }}
                      />
                    </label>
                    {banner.image_url && (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => void onDeleteImage(banner)}
                      >
                        ছবি মুছুন — Remove image
                      </Button>
                    )}
                    <Button
                      size="sm"
                      variant="danger"
                      onClick={() => void onDelete(banner)}
                    >
                      মুছুন — Delete
                    </Button>
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {total > PAGE_SIZE && (
        <p>
          মোট {total}টি — Total {total} · পৃষ্ঠা {page} /{" "}
          {Math.ceil(total / PAGE_SIZE)}{" "}
          <Button
            size="sm"
            variant="ghost"
            disabled={page <= 1}
            onClick={() => setPage((p) => Math.max(1, p - 1))}
          >
            ← আগের — Previous
          </Button>{" "}
          <Button
            size="sm"
            variant="ghost"
            disabled={page >= Math.ceil(total / PAGE_SIZE)}
            onClick={() => setPage((p) => p + 1)}
          >
            পরের — Next →
          </Button>
        </p>
      )}
    </AdminShell>
  );
}
