"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import {
  attachProductImageFromWeb,
  attachProductImageUrl,
  attachProductImageUrls,
  deleteProductImage,
  listProductImages,
  replaceProductImage,
  updateProductImages,
  uploadProductImageWithProgress,
  type ProductImageUpdate,
  type ProductImageView,
  type SellerVariant,
} from "@/lib/api";

/** One file in the upload queue, with its live progress. */
interface QueuedUpload {
  key: string;
  name: string;
  /** 0..1 while uploading; null once settled. */
  progress: number | null;
  error: string | null;
}

/**
 * ImageUploader (Wave 51): the seller's complete image manager
 * for ONE product — drag-and-drop or pick many files (each
 * uploads with its own progress bar through the media
 * pipeline), attach an image by URL, and manage what exists:
 * star = primary, ◀ ▶ reorder, alt-text editing, variant
 * assignment, delete with confirm. Every management action
 * goes through the ONE batch PATCH with the full list and
 * explicit primary flags, so the server's normalisation is
 * the only ordering logic — this component never invents its
 * own.
 */
export default function ImageUploader({
  token,
  productId,
  variants,
}: {
  token: string;
  productId: string;
  /** The product's variants, for the assignment select. */
  variants?: SellerVariant[];
}) {
  const [images, setImages] = useState<ProductImageView[]>([]);
  const [queue, setQueue] = useState<QueuedUpload[]>([]);
  const [urlField, setUrlField] = useState("");
  const [bulkField, setBulkField] = useState("");
  const [fetchField, setFetchField] = useState("");
  const [dragOver, setDragOver] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);

  const reload = useCallback(async () => {
    try {
      setImages(await listProductImages(token, productId));
    } catch (err) {
      setError(err instanceof Error ? err.message : "ছবি লোড হয়নি — Images failed to load");
    }
  }, [token, productId]);

  useEffect(() => {
    void reload();
  }, [reload]);

  /** Apply a batch update built from the CURRENT list by `build`. */
  const applyBatch = useCallback(
    async (build: (list: ProductImageView[]) => ProductImageUpdate[]) => {
      setBusy(true);
      setError(null);
      try {
        setImages(await updateProductImages(token, productId, build(images)));
      } catch (err) {
        setError(err instanceof Error ? err.message : "সংরক্ষণ হয়নি — Save failed");
      } finally {
        setBusy(false);
      }
    },
    [token, productId, images]
  );

  /** The full list as update items, with one image's fields changed. */
  const fullList = useCallback(
    (change: (image: ProductImageView) => ProductImageUpdate) =>
      images.map((image) => ({
        position: image.position,
        is_primary: image.is_primary,
        ...change(image),
        // The row's own id always wins over anything the change carries.
        id: image.id,
      })),
    [images]
  );

  const uploadFiles = useCallback(
    async (files: File[]) => {
      const accepted = files.filter((file) =>
        ["image/jpeg", "image/png", "image/webp"].includes(file.type)
      );
      if (accepted.length < files.length) {
        setError(
          "কিছু ফাইল ছবি নয় বলে বাদ গেছে — Some files were skipped: only JPEG, PNG or WebP images upload."
        );
      }
      for (const file of accepted) {
        const key = `${file.name}-${file.size}-${Date.now()}-${Math.random()}`;
        setQueue((current) => [
          ...current,
          { key, name: file.name, progress: 0, error: null },
        ]);
        try {
          const created = await uploadProductImageWithProgress(
            token,
            productId,
            file,
            (fraction) =>
              setQueue((current) =>
                current.map((item) =>
                  item.key === key ? { ...item, progress: fraction } : item
                )
              )
          );
          setImages((current) => [...current, created]);
          setQueue((current) => current.filter((item) => item.key !== key));
          setNotice("ছবি আপলোড হয়েছে — Image uploaded.");
        } catch (err) {
          setQueue((current) =>
            current.map((item) =>
              item.key === key
                ? {
                    ...item,
                    progress: null,
                    error:
                      err instanceof Error
                        ? err.message
                        : "আপলোড ব্যর্থ — Upload failed",
                  }
                : item
            )
          );
        }
      }
    },
    [token, productId]
  );

  const attachUrl = useCallback(async () => {
    const url = urlField.trim();
    if (!url) return;
    setBusy(true);
    setError(null);
    try {
      const created = await attachProductImageUrl(token, productId, url);
      setImages((current) => [...current, created]);
      setUrlField("");
      setNotice("URL থেকে ছবি যোগ হয়েছে — Image attached from URL.");
    } catch (err) {
      setError(err instanceof Error ? err.message : "URL যোগ হয়নি — Attach failed");
    } finally {
      setBusy(false);
    }
  }, [token, productId, urlField]);

  const attachBulkUrls = useCallback(async () => {
    const urls = bulkField
      .split(/\s+/)
      .map((part) => part.trim())
      .filter(Boolean);
    if (urls.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      const outcome = await attachProductImageUrls(token, productId, urls);
      if (outcome.created.length > 0) {
        setImages((current) => [...current, ...outcome.created]);
      }
      setBulkField("");
      if (outcome.failed.length > 0) {
        setError(outcome.failed.map((f) => `${f.filename}: ${f.error}`).join(" · "));
      } else {
        setNotice(
          `${outcome.created.length}টি ছবি যোগ হয়েছে — ${outcome.created.length} images attached.`
        );
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "URL যোগ হয়নি — Attach failed");
    } finally {
      setBusy(false);
    }
  }, [token, productId, bulkField]);

  const fetchFromWeb = useCallback(async () => {
    const url = fetchField.trim();
    if (!url) return;
    setBusy(true);
    setError(null);
    try {
      const created = await attachProductImageFromWeb(token, productId, url);
      setImages((current) => [...current, created]);
      setFetchField("");
      setNotice("ছবি এনে সংরক্ষণ হয়েছে — Image fetched and stored.");
    } catch (err) {
      setError(err instanceof Error ? err.message : "ছবি আনা যায়নি — Fetch failed");
    } finally {
      setBusy(false);
    }
  }, [token, productId, fetchField]);

  const replace = async (image: ProductImageView, file: File) => {
    setBusy(true);
    setError(null);
    try {
      const updated = await replaceProductImage(token, productId, image.id, file);
      setImages((current) =>
        current.map((item) => (item.id === updated.id ? updated : item))
      );
      setNotice("ছবি বদলানো হয়েছে — Image replaced.");
    } catch (err) {
      setError(err instanceof Error ? err.message : "বদলানো যায়নি — Replace failed");
    } finally {
      setBusy(false);
    }
  };

  const setPrimary = (imageId: string) =>
    applyBatch(() =>
      fullList((image) => ({ is_primary: image.id === imageId }))
    );

  const move = (index: number, delta: number) => {
    const target = index + delta;
    if (target < 0 || target >= images.length) return;
    const reordered = [...images];
    const [moved] = reordered.splice(index, 1);
    reordered.splice(target, 0, moved);
    void applyBatch(() =>
      reordered.map((image, position) => ({
        id: image.id,
        position,
        is_primary: image.is_primary,
      }))
    );
  };

  const saveAlt = (imageId: string, altText: string) =>
    applyBatch(() =>
      fullList((image) =>
        image.id === imageId ? { alt_text: altText.trim() || null } : {}
      )
    );

  const assignVariant = (imageId: string, variantId: string) =>
    applyBatch(() =>
      fullList((image) =>
        image.id === imageId ? { variant_id: variantId || null } : {}
      )
    );

  const remove = async (image: ProductImageView) => {
    if (
      !window.confirm(
        `এই ছবিটি মুছে ফেলবেন? — Delete this image${image.is_primary ? " (এটি প্রধান ছবি — it is the primary)" : ""}?`
      )
    ) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      setImages(await deleteProductImage(token, productId, image.id));
      setNotice("ছবি মোছা হয়েছে — Image deleted.");
    } catch (err) {
      setError(err instanceof Error ? err.message : "মোছা যায়নি — Delete failed");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="image-uploader" aria-label="পণ্যের ছবি — Product images">
      <h4>ছবি — Images ({images.length} / 12)</h4>

      <div
        className={
          dragOver ? "image-uploader-drop image-uploader-drop--over" : "image-uploader-drop"
        }
        tabIndex={0}
        onDragOver={(event) => {
          event.preventDefault();
          setDragOver(true);
        }}
        onDragLeave={() => setDragOver(false)}
        onDrop={(event) => {
          event.preventDefault();
          setDragOver(false);
          void uploadFiles(Array.from(event.dataTransfer.files));
        }}
        onPaste={(event) => {
          // Paste an image straight from the clipboard
          // (a screenshot, a copied photo) — the fastest
          // path from a phone gallery share sheet to here.
          const files = Array.from(event.clipboardData.files);
          if (files.length > 0) {
            event.preventDefault();
            void uploadFiles(files);
          }
        }}
      >
        <p>ছবি এখানে টেনে ছাড়ুন বা পেস্ট করুন — Drag &amp; drop or paste images here</p>
        <p className="image-uploader-hint">
          JPEG, PNG বা WebP, সর্বোচ্চ ১০ MB — up to 10 MB each. প্রথম ছবিটিই প্রধান
          ছবি হয় — the first image becomes the primary.
        </p>
        <button
          type="button"
          disabled={busy}
          onClick={() => fileInput.current?.click()}
        >
          ফাইল বেছে নিন — Choose files
        </button>
        <input
          ref={fileInput}
          type="file"
          accept="image/jpeg,image/png,image/webp"
          multiple
          hidden
          onChange={(event) => {
            const files = Array.from(event.target.files ?? []);
            event.target.value = "";
            void uploadFiles(files);
          }}
        />
      </div>

      {queue.length > 0 && (
        <ul className="image-uploader-queue">
          {queue.map((item) => (
            <li key={item.key}>
              <span>{item.name}</span>
              {item.error ? (
                <span className="image-uploader-queue-error">{item.error}</span>
              ) : (
                <progress
                  max={1}
                  value={item.progress ?? 0}
                  aria-label={`${item.name} আপলোড হচ্ছে`}
                />
              )}
            </li>
          ))}
        </ul>
      )}

      <div className="image-uploader-url">
        <label>
          ছবির URL — Image URL
          <input
            type="url"
            value={urlField}
            placeholder="https://…"
            onChange={(event) => setUrlField(event.target.value)}
          />
        </label>
        <button type="button" disabled={busy || !urlField.trim()} onClick={() => void attachUrl()}>
          URL যোগ করুন — Attach URL
        </button>
      </div>

      <div className="image-uploader-url">
        <label>
          একসাথে অনেক URL — Many URLs at once
          <textarea
            value={bulkField}
            rows={3}
            placeholder={"https://…/one.jpg\nhttps://…/two.jpg"}
            onChange={(event) => setBulkField(event.target.value)}
          />
        </label>
        <button
          type="button"
          disabled={busy || !bulkField.trim()}
          onClick={() => void attachBulkUrls()}
        >
          সব যোগ করুন — Attach all
        </button>
      </div>

      <div className="image-uploader-url">
        <label>
          ওয়েব থেকে এনে সংরক্ষণ — Fetch &amp; store from URL
          <input
            type="url"
            value={fetchField}
            placeholder="https://… (সার্ভার ছবিটি ডাউনলোড করে রাখবে)"
            onChange={(event) => setFetchField(event.target.value)}
          />
        </label>
        <button
          type="button"
          disabled={busy || !fetchField.trim()}
          onClick={() => void fetchFromWeb()}
        >
          এনে রাখুন — Fetch &amp; store
        </button>
      </div>

      {error && <p className="image-uploader-error" role="alert">{error}</p>}
      {notice && <p className="image-uploader-notice">{notice}</p>}

      {images.length === 0 ? (
        <p>এখনো কোনো ছবি নেই — No images yet. ছবি ছাড়া পণ্য প্রায় বিক্রি হয় না — products without photos rarely sell.</p>
      ) : (
        <ul className="image-uploader-grid">
          {images.map((image, index) => (
            <li key={image.id} className="image-uploader-item">
              {/* eslint-disable-next-line @next/next/no-img-element */}
              <img
                src={image.url_card}
                alt={image.alt_text ?? ""}
                loading="lazy"
                className="image-uploader-thumb"
              />
              <div className="image-uploader-item-meta">
                <span>
                  {image.source === "upload"
                    ? `${image.width ?? "?"}×${image.height ?? "?"}`
                    : "বাইরের URL — External"}
                  {image.is_primary ? " · ★ প্রধান — Primary" : ""}
                  {image.original_filename ? (
                    <>
                      <br />
                      {image.original_filename}
                    </>
                  ) : null}
                  {image.warnings.length > 0 ? (
                    <>
                      <br />
                      <span className="image-uploader-warning">
                        ⚠{" "}
                        {image.warnings
                          .map(
                            (code) =>
                              ({
                                low_resolution: "ছবিটি ছোট — Low resolution",
                                extreme_aspect: "অনুপাত বেশি চওড়া/লম্বা — Extreme aspect",
                                very_large: "ছবিটি অনেক বড় — Very large",
                              })[code] ?? code
                          )
                          .join(" · ")}
                      </span>
                    </>
                  ) : null}
                </span>
                <div className="image-uploader-actions">
                  <button
                    type="button"
                    disabled={busy || image.is_primary}
                    title="প্রধান ছবি করুন — Make primary"
                    onClick={() => void setPrimary(image.id)}
                  >
                    ★
                  </button>
                  <button
                    type="button"
                    disabled={busy || index === 0}
                    title="বামে সরান — Move left"
                    onClick={() => move(index, -1)}
                  >
                    ◀
                  </button>
                  <button
                    type="button"
                    disabled={busy || index === images.length - 1}
                    title="ডানে সরান — Move right"
                    onClick={() => move(index, 1)}
                  >
                    ▶
                  </button>
                  <button
                    type="button"
                    disabled={busy}
                    title="মুছুন — Delete"
                    onClick={() => void remove(image)}
                  >
                    🗑
                  </button>
                  {image.source === "upload" &&
                  image.url_original.startsWith("/media/") ? (
                    <a
                      href={`${image.url_original}?download=1`}
                      title="মূল ছবি নামান — Download original"
                      download
                    >
                      ⬇
                    </a>
                  ) : null}
                </div>
              </div>
              <label className="image-uploader-alt">
                বিকল্প লেখা — Alt text
                <input
                  type="text"
                  defaultValue={image.alt_text ?? ""}
                  maxLength={200}
                  placeholder="ছবির বর্ণনা — Describe the photo"
                  onBlur={(event) => {
                    if ((event.target.value.trim() || null) !== image.alt_text) {
                      void saveAlt(image.id, event.target.value);
                    }
                  }}
                />
              </label>
              {image.source === "upload" && (
                <label className="image-uploader-replace">
                  ছবি বদলান — Replace photo
                  <input
                    type="file"
                    accept="image/jpeg,image/png,image/webp"
                    disabled={busy}
                    onChange={(event) => {
                      const file = event.target.files?.[0];
                      event.target.value = "";
                      if (file) void replace(image, file);
                    }}
                  />
                </label>
              )}
              {variants && variants.length > 0 && (
                <label className="image-uploader-variant">
                  ভ্যারিয়েন্ট — Variant
                  <select
                    value={image.variant_id ?? ""}
                    disabled={busy}
                    onChange={(event) => void assignVariant(image.id, event.target.value)}
                  >
                    <option value="">— কোনোটিই নয় — None —</option>
                    {variants.map((variant) => (
                      <option key={variant.id} value={variant.id}>
                        {variant.name}
                      </option>
                    ))}
                  </select>
                </label>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
