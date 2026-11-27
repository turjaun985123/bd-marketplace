"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  getSellerDashboard,
  updateStoreBranding,
  uploadBrandingImage,
  type Store,
  type StoreTheme,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/** The three themes with their Bangla names + one-line descriptions. */
const THEMES: { key: StoreTheme; name: string; blurb: string }[] = [
  {
    key: "classic",
    name: "ক্লাসিক (Classic)",
    blurb: "ব্যানার ছবি ও লোগোসহ চেনা সাজ — আগের মতোই স্টোরফ্রন্ট।",
  },
  {
    key: "minimal",
    name: "মিনিমাল (Minimal)",
    blurb: "ব্যানার ছাড়া হালকা সাজ — পণ্য ছোট ছোট সারিতে, কম রঙে।",
  },
  {
    key: "bold",
    name: "বোল্ড (Bold)",
    blurb: "বড় ব্যানার, বড় স্টোরের নাম, দুই কলামে বড় পণ্যের কার্ড।",
  },
];

/** Character count the way the API counts: Unicode characters. */
function charCount(text: string): number {
  return Array.from(text).length;
}

/**
 * Seller branding (Wave 18): theme picker, brand colour, logo /
 * banner URLs and the announcement for the seller's own store,
 * saved through PUT /v1/seller/store/branding.
 *
 * The form loads the store from the seller dashboard and sends
 * EVERY field on save, which matches the API's PATCH convention:
 * a field left empty CLEARS it (logo, banner, colour,
 * announcement, description), a filled field is validated by
 * the API and stored. Image URLs are hotlinked — there is no
 * upload yet — and the previews below are the seller's own
 * browser loading those same URLs.
 */
export default function SellerBrandingPage() {
  const { ready, token } = useAuth();
  const [store, setStore] = useState<Store | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [theme, setTheme] = useState<StoreTheme>("classic");
  const [brandColor, setBrandColor] = useState("");
  const [logoUrl, setLogoUrl] = useState("");
  const [bannerUrl, setBannerUrl] = useState("");
  const [announcement, setAnnouncement] = useState("");
  const [description, setDescription] = useState("");

  const fillForm = useCallback((s: Store) => {
    setStore(s);
    setTheme(s.theme_key);
    setBrandColor(s.brand_color ?? "");
    setLogoUrl(s.logo_url ?? "");
    setBannerUrl(s.banner_url ?? "");
    setAnnouncement(s.announcement ?? "");
    setDescription(s.description ?? "");
  }, []);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    getSellerDashboard(token)
      .then((dashboard) => {
        if (!cancelled) fillForm(dashboard.store);
      })
      .catch((err) => {
        if (!cancelled) {
          setMessage(
            err instanceof Error ? err.message : "স্টোর লোড করা যায়নি।"
          );
        }
      });
    return () => {
      cancelled = true;
    };
  }, [token, fillForm]);

  // Wave 51: upload a logo/banner file through the media
  // pipeline; the API stores it and points the store's
  // logo_url / banner_url at the served file, and the form
  // refills from the returned store.
  async function onUploadBranding(kind: "logo" | "banner", file: File) {
    if (!token) return;
    setBusy(true);
    setMessage(null);
    try {
      const updated = await uploadBrandingImage(token, kind, file);
      fillForm(updated);
      setMessage(
        kind === "logo"
          ? "লোগো আপলোড হয়েছে। (Logo uploaded.)"
          : "ব্যানার আপলোড হয়েছে। (Banner uploaded.)"
      );
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "আপলোড করা যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  async function onSave(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setBusy(true);
    setMessage(null);
    try {
      const saved = await updateStoreBranding(token, {
        theme_key: theme,
        brand_color: brandColor.trim(),
        logo_url: logoUrl.trim(),
        banner_url: bannerUrl.trim(),
        announcement: announcement.trim(),
        description: description.trim(),
      });
      fillForm(saved);
      setMessage("স্টোরের সাজসজ্জা সেভ হয়েছে। (Branding saved.)");
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "সেভ করা যায়নি।");
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
        <h1>স্টোরের সাজসজ্জা</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  // The colour picker needs a #rrggbb value; fall back to black
  // while the hex text is empty or mid-typing.
  const pickerValue = /^#[0-9a-fA-F]{6}$/.test(brandColor.trim())
    ? brandColor.trim().toLowerCase()
    : "#000000";

  return (
    <main>
      <h1>স্টোরের সাজসজ্জা — Store branding</h1>
      <p>
        <Link href="/seller">← ড্যাশবোর্ডে ফিরুন — Dashboard</Link>
        {store && (
          <>
            {" · "}
            <Link href={`/${store.slug}`}>
              আমার স্টোরফ্রন্ট দেখুন — View storefront
            </Link>
          </>
        )}
      </p>
      {message && <p role="status">{message}</p>}
      {store === null ? (
        <p>লোড হচ্ছে…</p>
      ) : (
        <form onSubmit={(e) => void onSave(e)}>
          <h2>থিম — Theme</h2>
          {THEMES.map((t) => (
            <label key={t.key} className="card">
              <input
                type="radio"
                name="theme"
                value={t.key}
                checked={theme === t.key}
                onChange={() => setTheme(t.key)}
              />{" "}
              <strong>{t.name}</strong>
              <br />
              {t.blurb}
            </label>
          ))}

          <h2>ব্র্যান্ডের রঙ — Brand colour</h2>
          <p>
            ঘোষণার বার ও বোতামের কিনারায় এই রঙটি অ্যাকসেন্ট হিসেবে
            ব্যবহৃত হয়। খালি রাখলে রঙ মুছে যাবে।
          </p>
          <label>
            রঙ বাছুন (colour picker)
            <input
              type="color"
              value={pickerValue}
              onChange={(e) => setBrandColor(e.target.value.toUpperCase())}
            />
          </label>
          <label>
            হেক্স রঙ (hex, যেমন #FF5722)
            <input
              value={brandColor}
              onChange={(e) => setBrandColor(e.target.value)}
              placeholder="#FF5722"
            />
          </label>

          <h2>লোগো ও ব্যানার — Logo &amp; banner</h2>
          <p>
            ছবি আপলোড করুন (Wave 51 — ফাইলটি মিডিয়া পাইপলাইনে
            সংরক্ষিত হয়), অথবা ছবির URL দিন (https:// দিয়ে শুরু হতে
            হবে — URL-এর ছবিটিই সরাসরি দেখানো হয়)। URL খালি রাখলে
            ছবি মুছে যাবে।
          </p>
          <label>
            লোগোর URL (logo URL)
            <input
              value={logoUrl}
              onChange={(e) => setLogoUrl(e.target.value)}
              placeholder="https://example.com/logo.png"
            />
          </label>
          {logoUrl.trim() && (
            <p>
              {/* eslint-disable-next-line @next/next/no-img-element */}
              <img
                src={logoUrl.trim()}
                alt="লোগোর প্রিভিউ"
                style={{ height: 56 }}
              />
            </p>
          )}
          <label>
            লোগো আপলোড — Upload logo
            <input
              type="file"
              accept="image/jpeg,image/png,image/webp"
              onChange={(e) => {
                const file = e.target.files?.[0];
                e.target.value = "";
                if (file) void onUploadBranding("logo", file);
              }}
            />
          </label>
          <label>
            ব্যানারের URL (banner URL)
            <input
              value={bannerUrl}
              onChange={(e) => setBannerUrl(e.target.value)}
              placeholder="https://example.com/banner.jpg"
            />
          </label>
          {bannerUrl.trim() && (
            <p>
              {/* eslint-disable-next-line @next/next/no-img-element */}
              <img
                src={bannerUrl.trim()}
                alt="ব্যানারের প্রিভিউ"
                style={{ width: "100%", maxHeight: 180, objectFit: "cover" }}
              />
            </p>
          )}
          <label>
            ব্যানার আপলোড — Upload banner
            <input
              type="file"
              accept="image/jpeg,image/png,image/webp"
              onChange={(e) => {
                const file = e.target.files?.[0];
                e.target.value = "";
                if (file) void onUploadBranding("banner", file);
              }}
            />
          </label>

          <h2>ঘোষণা — Announcement</h2>
          <label>
            স্টোরফ্রন্টের ওপরে এক লাইনের ঘোষণা (সর্বোচ্চ ১২০ অক্ষর) —{" "}
            {charCount(announcement)}/120
            <input
              value={announcement}
              onChange={(e) => setAnnouncement(e.target.value)}
              placeholder="ঈদের অফার চলছে!"
            />
          </label>

          <h2>স্টোরের বর্ণনা — Description</h2>
          <label>
            স্টোর সম্পর্কে সংক্ষেপে (সর্বোচ্চ ১০০০ অক্ষর) —{" "}
            {charCount(description)}/1000
            <textarea
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              rows={4}
              cols={60}
            />
          </label>

          <p>
            <button type="submit" disabled={busy}>
              {busy ? "সেভ হচ্ছে…" : "সেভ করুন — Save branding"}
            </button>
          </p>
        </form>
      )}
    </main>
  );
}
