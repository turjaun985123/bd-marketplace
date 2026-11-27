"use client";

/**
 * Shared UI primitives (Wave 52) — the design system's
 * building blocks, built once and used across the
 * marketplace, storefronts, seller and admin surfaces:
 * buttons, badges, section headers, rails, skeletons,
 * empty states and the countdown. Everything is styled
 * from the token layer in globals.css (the `--brand-*` /
 * `--surface` / `--radius-*` custom properties); nothing
 * here hard-codes a colour. Client components as a group
 * (the countdown ticks), which is safe: every consumer of
 * these primitives is an interactive surface.
 */

import Link from "next/link";
import { useEffect, useRef, useState, type ReactNode } from "react";

// ---------------------------------------------------------------------------
// Button
// ---------------------------------------------------------------------------

export type ButtonVariant = "primary" | "default" | "ghost" | "danger";
export type ButtonSize = "sm" | "md" | "lg";

export interface ButtonProps {
  children: ReactNode;
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** Render a spinner label state and block the click. */
  loading?: boolean;
  disabled?: boolean;
  /** When set, renders a link styled as the button. */
  href?: string;
  type?: "button" | "submit";
  block?: boolean;
  onClick?: () => void;
}

/**
 * The one button. `href` renders a Next link with the same
 * classes (call-to-action links and buttons must look
 * identical — Daraz/Shopify never let you tell them apart).
 */
export function Button({
  children,
  variant = "default",
  size = "md",
  loading = false,
  disabled = false,
  href,
  type = "button",
  block = false,
  onClick,
}: ButtonProps) {
  const className = [
    "btn",
    variant !== "default" ? `btn--${variant}` : "",
    size !== "md" ? `btn--${size}` : "",
    block ? "btn--block" : "",
  ]
    .filter(Boolean)
    .join(" ");
  const content = loading ? "লোড হচ্ছে…" : children;
  if (href && !disabled && !loading) {
    return (
      <Link href={href} className={className} onClick={onClick}>
        {content}
      </Link>
    );
  }
  return (
    <button
      type={type}
      className={className}
      disabled={disabled || loading}
      onClick={onClick}
    >
      {content}
    </button>
  );
}

// ---------------------------------------------------------------------------
// Badge
// ---------------------------------------------------------------------------

export type BadgeTone = "brand" | "accent" | "danger" | "muted" | "flash";

/** A small pill label. Tones map to token colours only. */
export function Badge({
  tone = "muted",
  children,
}: {
  tone?: BadgeTone;
  children: ReactNode;
}) {
  return <span className={`badge badge--${tone}`}>{children}</span>;
}

// ---------------------------------------------------------------------------
// SectionHeader
// ---------------------------------------------------------------------------

/**
 * A home/feed section's heading row: the title and an
 * optional "see all" link, with the shared spacing.
 */
export function SectionHeader({
  title,
  linkHref,
  linkLabel,
  tone,
}: {
  title: string;
  linkHref?: string;
  linkLabel?: string;
  /** "flash" paints the title in the urgency accent. */
  tone?: "flash";
}) {
  return (
    <div className="section-header">
      <h2 className={tone === "flash" ? "flash-title" : undefined}>{title}</h2>
      {linkHref ? (
        <Link href={linkHref} className="section-header-link">
          {linkLabel ?? "সব দেখুন — See all"} →
        </Link>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Skeleton
// ---------------------------------------------------------------------------

/** A shimmering placeholder block (token-coloured). */
export function Skeleton({
  width,
  height = 14,
  className = "",
}: {
  width?: string | number;
  height?: string | number;
  className?: string;
}) {
  return (
    <div
      className={`skeleton ${className}`}
      style={{ width, height }}
      aria-hidden="true"
    />
  );
}

/**
 * The product-card-shaped skeleton: image block + title,
 * store and price lines at the rail card width. Feed
 * sections render a row of these while the feed loads, so
 * the page never jumps when the cards arrive.
 */
export function ProductCardSkeleton() {
  return (
    <div className="skeleton-card" aria-hidden="true">
      <Skeleton height={150} />
      <div className="skeleton skeleton-line" />
      <div className="skeleton skeleton-line skeleton-line--short" />
      <div className="skeleton skeleton-line skeleton-line--price" />
    </div>
  );
}

/** A rail of product-card skeletons. */
export function ProductRailSkeleton({ count = 4 }: { count?: number }) {
  return (
    <div className="rail rail--cards" aria-hidden="true">
      {Array.from({ length: count }, (_, i) => (
        <ProductCardSkeleton key={i} />
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// EmptyState
// ---------------------------------------------------------------------------

/**
 * The honest empty section: a typographic panel (no icon
 * font on this platform) saying what would appear here and,
 * when there is one, the action that fills it.
 */
export function EmptyState({
  title,
  children,
  action,
}: {
  title: string;
  children?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="empty-state">
      <p className="empty-state-title">{title}</p>
      {children}
      {action}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Rail
// ---------------------------------------------------------------------------

/**
 * A horizontal scroll-snap row — the one rendering for
 * card rails across the home feed. `cards` fixes the child
 * width to the product-card rail width. The arrow buttons
 * scroll the row by most of a viewport (they are the
 * desktop affordance; touch scrolls natively, and the
 * buttons are hidden on coarse pointers by the stylesheet
 * where they would only crowd the cards).
 */
export function Rail({
  children,
  cards = false,
}: {
  children: ReactNode;
  cards?: boolean;
}) {
  const scroller = useRef<HTMLDivElement | null>(null);

  function scrollByPage(direction: 1 | -1) {
    const el = scroller.current;
    if (!el) return;
    el.scrollBy({ left: direction * el.clientWidth * 0.85, behavior: "smooth" });
  }

  return (
    <div className="rail-wrap">
      <div
        ref={scroller}
        className={cards ? "rail rail--cards" : "rail"}
      >
        {children}
      </div>
      <button
        type="button"
        className="rail-arrow rail-arrow--prev"
        onClick={() => scrollByPage(-1)}
        aria-label="আগের পণ্য — Scroll back"
      >
        ‹
      </button>
      <button
        type="button"
        className="rail-arrow rail-arrow--next"
        onClick={() => scrollByPage(1)}
        aria-label="পরের পণ্য — Scroll forward"
      >
        ›
      </button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Countdown
// ---------------------------------------------------------------------------

/** Split a remaining-seconds count into d/h/m/s cells. */
export function countdownParts(totalSeconds: number): {
  days: number;
  hours: number;
  minutes: number;
  seconds: number;
} {
  const clamped = Math.max(0, Math.floor(totalSeconds));
  return {
    days: Math.floor(clamped / 86_400),
    hours: Math.floor((clamped % 86_400) / 3_600),
    minutes: Math.floor((clamped % 3_600) / 60),
    seconds: clamped % 60,
  };
}

function pad2(value: number): string {
  return String(value).padStart(2, "0");
}

/**
 * A live countdown to an ISO timestamp, ticking once a
 * second. Renders the remaining time as boxed cells with
 * Bangla unit labels; at (and past) the deadline it renders
 * the ended label once and stops ticking — a flash section
 * whose sale ended must not pretend otherwise (the feed
 * itself will drop the section on its next load).
 */
export function Countdown({
  endsAt,
  endedLabel = "শেষ হয়ে গেছে — Ended",
}: {
  endsAt: string;
  endedLabel?: string;
}) {
  const [nowMs, setNowMs] = useState<number | null>(null);

  useEffect(() => {
    setNowMs(Date.now());
    const timer = setInterval(() => setNowMs(Date.now()), 1_000);
    return () => clearInterval(timer);
  }, []);

  if (nowMs === null) {
    // First paint (and SSR): a neutral placeholder with the
    // same footprint, replaced by the real remaining time
    // on mount — never a wrong number.
    return (
      <span className="countdown" aria-label="countdown">
        <span className="countdown-cell">--</span>
      </span>
    );
  }
  const remaining = (new Date(endsAt).getTime() - nowMs) / 1_000;
  if (remaining <= 0) {
    return <span className="countdown">{endedLabel}</span>;
  }
  const parts = countdownParts(remaining);
  const cells: { value: string; label: string }[] = [];
  if (parts.days > 0) cells.push({ value: String(parts.days), label: "দিন" });
  cells.push({ value: pad2(parts.hours), label: "ঘণ্টা" });
  cells.push({ value: pad2(parts.minutes), label: "মিনিট" });
  cells.push({ value: pad2(parts.seconds), label: "সেকেন্ড" });
  return (
    <span className="countdown" role="timer" aria-label="Sale ends in">
      {cells.map((cell, i) => (
        <span key={cell.label}>
          <span className="countdown-cell">{cell.value}</span>
          <span className="countdown-label">{cell.label}</span>
          {i < cells.length - 1 ? <span className="countdown-sep">:</span> : null}
        </span>
      ))}
    </span>
  );
}
