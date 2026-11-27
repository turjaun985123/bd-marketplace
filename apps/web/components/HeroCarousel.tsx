"use client";

import Link from "next/link";
import { useCallback, useEffect, useRef, useState } from "react";
import { recordMerchEvent, type HomeBanner } from "@/lib/api";

/**
 * The home page hero carousel (Wave 52): the admin-managed
 * banners as full-bleed slides — image, headline, subtitle
 * and the banner's link as the call to action. Auto-advances
 * every five seconds (pausing while hovered or touched),
 * with dots, arrows and touch swipe. A banner without an
 * image renders as a branded text slide; a banner without a
 * link renders as a plain slide, not a dead anchor.
 *
 * Every click reports the banner_click merchandising event
 * (fire-and-forget — navigation never waits on it).
 */
export default function HeroCarousel({ banners }: { banners: HomeBanner[] }) {
  const [index, setIndex] = useState(0);
  const [paused, setPaused] = useState(false);
  const touchX = useRef<number | null>(null);
  const count = banners.length;

  const go = useCallback(
    (next: number) => {
      if (count === 0) return;
      setIndex(((next % count) + count) % count);
    },
    [count]
  );

  useEffect(() => {
    if (count <= 1 || paused) return;
    const timer = setInterval(() => {
      setIndex((current) => (current + 1) % count);
    }, 5_000);
    return () => clearInterval(timer);
  }, [count, paused]);

  if (count === 0) return null;
  const active = Math.min(index, count - 1);

  return (
    <div
      className="hero-carousel"
      onMouseEnter={() => setPaused(true)}
      onMouseLeave={() => setPaused(false)}
      onTouchStart={(event) => {
        setPaused(true);
        touchX.current = event.touches[0]?.clientX ?? null;
      }}
      onTouchEnd={(event) => {
        setPaused(false);
        const start = touchX.current;
        touchX.current = null;
        if (start === null) return;
        const end = event.changedTouches[0]?.clientX ?? start;
        if (end - start > 40) go(active - 1);
        else if (start - end > 40) go(active + 1);
      }}
      aria-roledescription="carousel"
      aria-label="প্রচারণা — Campaigns"
    >
      {banners.map((banner, i) => {
        const body = (
          <>
            {banner.image_url ? (
              // eslint-disable-next-line @next/next/no-img-element
              <img
                src={banner.image_url}
                alt={banner.title}
                className="hero-slide-image"
                loading={i === 0 ? "eager" : "lazy"}
              />
            ) : null}
            <span
              className={
                banner.image_url ? "hero-slide-body" : "hero-slide-body hero-slide-body--plain"
              }
            >
              <span className="hero-slide-title">{banner.title}</span>
              {banner.subtitle ? (
                <span className="hero-slide-subtitle" style={{ display: "block" }}>
                  {banner.subtitle}
                </span>
              ) : null}
              {banner.link_url ? (
                <span className="btn btn--primary">কিনুন — Shop now</span>
              ) : null}
            </span>
          </>
        );
        const className = i === active ? "hero-slide hero-slide--active" : "hero-slide";
        return banner.link_url ? (
          <Link
            key={banner.id}
            href={banner.link_url}
            className={className}
            onClick={() => recordMerchEvent("banner_click", banner.id)}
            aria-hidden={i !== active}
            tabIndex={i === active ? 0 : -1}
          >
            {body}
          </Link>
        ) : (
          <div key={banner.id} className={className} aria-hidden={i !== active}>
            {body}
          </div>
        );
      })}
      {count > 1 && (
        <>
          <button
            type="button"
            className="hero-arrow hero-arrow--prev"
            onClick={() => go(active - 1)}
            aria-label="আগের ব্যানার — Previous banner"
          >
            ‹
          </button>
          <button
            type="button"
            className="hero-arrow hero-arrow--next"
            onClick={() => go(active + 1)}
            aria-label="পরের ব্যানার — Next banner"
          >
            ›
          </button>
          <div className="hero-dots">
            {banners.map((banner, i) => (
              <button
                key={banner.id}
                type="button"
                className={i === active ? "hero-dot hero-dot--active" : "hero-dot"}
                onClick={() => go(i)}
                aria-label={`ব্যানার ${i + 1} — Banner ${i + 1}`}
              />
            ))}
          </div>
        </>
      )}
    </div>
  );
}
