"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import type { GalleryImageView } from "@/lib/api";
import ProductImage from "./ProductImage";

/**
 * ProductGallery (Wave 51): the marketplace detail page's
 * image stage — one large image, a thumbnail strip, prev/next
 * controls, keyboard navigation (arrow keys move, Enter opens
 * the lightbox, Escape closes it), and a lightbox overlay
 * showing the zoom size. A product with no images renders the
 * shared initial-letter placeholder, so the layout never
 * collapses.
 */
export default function ProductGallery({
  images: allImages,
  title,
  focusUrl,
  variantId,
}: {
  images: GalleryImageView[];
  title: string;
  /**
   * Wave 51 extension: when this names one of the gallery
   * images (by its gallery URL — the variant picker's
   * `image_url`), the stage jumps to it. The detail page
   * passes the selected variant's image, so picking a
   * colour shows that colour's photo.
   */
  focusUrl?: string | null;
  /**
   * Wave 51 extension: the selected variant's id. When
   * set, images assigned to that variant lead the strip
   * (stable order otherwise) — picking a colour leads
   * with that colour's photos, Shopify-style.
   */
  variantId?: string | null;
}) {
  const images =
    variantId == null
      ? allImages
      : [
          ...allImages.filter((image) => image.variant_id === variantId),
          ...allImages.filter((image) => image.variant_id !== variantId),
        ];
  const [active, setActive] = useState(0);
  const [lightbox, setLightbox] = useState(false);
  const [showAll, setShowAll] = useState(false);
  // Hover-zoom (Wave 51 extension): while a fine pointer
  // hovers the stage, the image magnifies around the
  // pointer — the marketplace "inspect the stitching"
  // gesture. The origin is a percent pair over the stage.
  const [zoomOrigin, setZoomOrigin] = useState<string | null>(null);
  const touchStartX = useRef<number | null>(null);
  const lightboxRef = useRef<HTMLDivElement>(null);

  // Follow the focus URL (variant selection) when it
  // names a gallery image.
  useEffect(() => {
    if (!focusUrl) return;
    const index = images.findIndex((image) => image.url_gallery === focusUrl);
    if (index >= 0) setActive(index);
  }, [focusUrl, images]);

  const toggleFullscreen = useCallback(() => {
    const node = lightboxRef.current;
    if (!node) return;
    if (document.fullscreenElement) {
      void document.exitFullscreen();
    } else if (node.requestFullscreen) {
      void node.requestFullscreen();
    }
  }, []);

  const count = images.length;
  const go = useCallback(
    (delta: number) => {
      setActive((current) => {
        if (count === 0) return 0;
        return (current + delta + count) % count;
      });
    },
    [count]
  );

  // Lightbox keyboard control: arrows move, Escape closes.
  useEffect(() => {
    if (!lightbox) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setLightbox(false);
      if (event.key === "ArrowLeft") go(-1);
      if (event.key === "ArrowRight") go(1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [lightbox, go]);

  if (count === 0) {
    return (
      <div className="product-gallery">
        <ProductImage
          src={null}
          alt={title}
          placeholderText={title}
          className="product-gallery-stage-image"
        />
      </div>
    );
  }

  const current = images[Math.min(active, count - 1)];
  const srcSet = [
    `${current.url_card} 300w`,
    `${current.url_gallery} 600w`,
    `${current.url_zoom} 1200w`,
  ]
    // External images repeat one URL at every size — a srcset
    // with three identical candidates is noise, so collapse it.
    .filter((candidate, index, all) => all.indexOf(candidate) === index)
    .join(", ");

  return (
    <div className="product-gallery">
      <div
        className="product-gallery-stage"
        tabIndex={0}
        role="group"
        aria-label={`${title} — ছবি ${active + 1} / ${count}`}
        onKeyDown={(event) => {
          if (event.key === "ArrowLeft") go(-1);
          if (event.key === "ArrowRight") go(1);
          if (event.key === "Home") setActive(0);
          if (event.key === "End") setActive(count - 1);
          if (event.key === "Enter") setLightbox(true);
        }}
        onMouseMove={(event) => {
          const rect = event.currentTarget.getBoundingClientRect();
          const x = ((event.clientX - rect.left) / rect.width) * 100;
          const y = ((event.clientY - rect.top) / rect.height) * 100;
          setZoomOrigin(`${x.toFixed(1)}% ${y.toFixed(1)}%`);
        }}
        onMouseLeave={() => setZoomOrigin(null)}
        onTouchStart={(event) => {
          touchStartX.current = event.touches[0]?.clientX ?? null;
        }}
        onTouchEnd={(event) => {
          const start = touchStartX.current;
          touchStartX.current = null;
          const end = event.changedTouches[0]?.clientX;
          if (start === null || end === undefined) return;
          const delta = end - start;
          // A deliberate swipe (40px+) moves one image;
          // smaller movements are taps and scrolls.
          if (delta <= -40) go(1);
          if (delta >= 40) go(-1);
        }}
      >
        <div
          className={
            zoomOrigin
              ? "product-gallery-zoom-frame product-gallery-zoom-frame--on"
              : "product-gallery-zoom-frame"
          }
          style={zoomOrigin ? { transformOrigin: zoomOrigin } : undefined}
        >
          <ProductImage
            src={current.url_gallery}
            srcSet={srcSet}
            sizes="(max-width: 700px) 100vw, 600px"
            alt={current.alt_text ?? title}
            placeholderText={title}
            className="product-gallery-stage-image"
          />
        </div>
        <span className="product-gallery-live" aria-live="polite">
          ছবি {active + 1} / {count}
        </span>
        {count > 1 && (
          <>
            <button
              type="button"
              className="product-gallery-nav product-gallery-nav--prev"
              aria-label="আগের ছবি — Previous image"
              onClick={() => go(-1)}
            >
              ‹
            </button>
            <button
              type="button"
              className="product-gallery-nav product-gallery-nav--next"
              aria-label="পরের ছবি — Next image"
              onClick={() => go(1)}
            >
              ›
            </button>
          </>
        )}
        <button
          type="button"
          className="product-gallery-zoom"
          onClick={() => setLightbox(true)}
        >
          🔍 বড় করে দেখুন — Zoom
        </button>
        {count > 1 && (
          <button
            type="button"
            className="product-gallery-all"
            onClick={() => setShowAll((current) => !current)}
            aria-expanded={showAll}
          >
            🗔 সব ছবি ({count}) — All photos
          </button>
        )}
        {current.is_primary && (
          <span className="product-gallery-primary-badge">
            প্রধান ছবি — Main photo
          </span>
        )}
      </div>

      {count > 1 && (
        <div className="product-gallery-thumbs" role="tablist" aria-label="ছবির তালিকা — Images">
          {images.map((image, index) => (
            <button
              key={image.id}
              type="button"
              role="tab"
              aria-selected={index === active}
              ref={
                index === active
                  ? (node) => {
                      // Keep the active thumb visible in a
                      // long strip (12-image galleries scroll).
                      node?.scrollIntoView({
                        block: "nearest",
                        inline: "nearest",
                      });
                    }
                  : undefined
              }
              className={
                index === active
                  ? "product-gallery-thumb product-gallery-thumb--active"
                  : focusUrl && image.url_gallery === focusUrl
                    ? "product-gallery-thumb product-gallery-thumb--variant"
                    : "product-gallery-thumb"
              }
              onClick={() => setActive(index)}
            >
              <ProductImage
                src={image.url_card}
                alt={image.alt_text ?? `${title} — ছবি ${index + 1}`}
                placeholderText={title}
              />
            </button>
          ))}
        </div>
      )}

      {showAll && (
        <div className="product-gallery-all-grid" aria-label="সব ছবি — All photos">
          {images.map((image, index) => (
            <button
              key={image.id}
              type="button"
              className={
                index === active
                  ? "product-gallery-all-item product-gallery-all-item--active"
                  : "product-gallery-all-item"
              }
              onClick={() => {
                setActive(index);
                setShowAll(false);
              }}
            >
              <ProductImage
                src={image.url_card}
                alt={image.alt_text ?? `${title} — ছবি ${index + 1}`}
                placeholderText={title}
                className="product-gallery-all-image"
              />
              <span>
                {index + 1}
                {image.is_primary ? " · ★" : ""}
                {image.variant_id ? " · ভ্যারিয়েন্ট" : ""}
              </span>
            </button>
          ))}
        </div>
      )}

      {lightbox && (
        <div
          ref={lightboxRef}
          className="product-gallery-lightbox"
          role="dialog"
          aria-modal="true"
          aria-label={`${title} — বড় ছবি`}
          onClick={() => setLightbox(false)}
        >
          <button
            type="button"
            className="product-gallery-lightbox-close"
            aria-label="বন্ধ করুন — Close"
            onClick={() => setLightbox(false)}
          >
            ✕
          </button>
          <button
            type="button"
            className="product-gallery-lightbox-fullscreen"
            aria-label="পূর্ণ পর্দা — Fullscreen"
            onClick={(event) => {
              event.stopPropagation();
              toggleFullscreen();
            }}
          >
            ⛶
          </button>
          {count > 1 && (
            <>
              <button
                type="button"
                className="product-gallery-nav product-gallery-nav--prev"
                aria-label="আগের ছবি — Previous image"
                onClick={(event) => {
                  event.stopPropagation();
                  go(-1);
                }}
              >
                ‹
              </button>
              <button
                type="button"
                className="product-gallery-nav product-gallery-nav--next"
                aria-label="পরের ছবি — Next image"
                onClick={(event) => {
                  event.stopPropagation();
                  go(1);
                }}
              >
                ›
              </button>
            </>
          )}
          {/* The zoom view is the point of the lightbox; a
              plain <img> at the API's largest derived size. */}
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img
            className="product-gallery-lightbox-image"
            src={current.url_zoom}
            alt={current.alt_text ?? title}
            onClick={(event) => event.stopPropagation()}
          />
          <p className="product-gallery-lightbox-caption">
            {active + 1} / {count}
            {current.alt_text ? ` — ${current.alt_text}` : ""}
          </p>
        </div>
      )}
    </div>
  );
}
