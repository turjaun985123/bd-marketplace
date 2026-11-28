/**
 * Shared formatting helpers — built once, reused everywhere.
 * Money is integer poisha (1 BDT = 100 poisha), matching the Rust domain crate.
 */
export function formatBdt(poisha: number): string {
  const sign = poisha < 0 ? "-" : "";
  const abs = Math.abs(poisha);
  const taka = Math.floor(abs / 100);
  const remainder = abs % 100;
  const takaText = taka.toLocaleString("en-IN");
  return remainder === 0
    ? `${sign}৳${takaText}`
    : `${sign}৳${takaText}.${String(remainder).padStart(2, "0")}`;
}

/**
 * Parse a taka amount typed by a user ("1500", "1,500", "1500.50")
 * into integer poisha for the API, which only ever speaks poisha.
 * Returns null for anything that is not a non-negative amount with
 * at most two decimal places — callers show a validation message
 * instead of guessing.
 */
export function parseTakaToPoisha(input: string): number | null {
  const cleaned = input.replace(/[,\s]/g, "");
  if (!/^\d+(\.\d{1,2})?$/.test(cleaned)) return null;
  const [takaPart, poishaPart = ""] = cleaned.split(".");
  const poisha = Number(takaPart) * 100 + Number(poishaPart.padEnd(2, "0"));
  return Number.isSafeInteger(poisha) ? poisha : null;
}

/**
 * Convert an API ISO timestamp to the value a
 * `datetime-local` input displays (the browser's local
 * time, minute precision). Empty string for null/invalid —
 * the input simply starts blank.
 */
export function isoToLocalInput(iso: string | null | undefined): string {
  if (!iso) return "";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}` +
    `T${pad(date.getHours())}:${pad(date.getMinutes())}`
  );
}

/**
 * Convert a `datetime-local` input value back to the ISO
 * timestamp the API stores. Null for an empty or invalid
 * input — callers treat null as "no bound" (banner windows)
 * or refuse the save (flash sale windows, where both bounds
 * are required).
 */
export function localInputToIso(value: string): string | null {
  if (!value) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

/** A stored ISO timestamp as a short local date-time line. */
export function formatDateTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleString("en-GB", {
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/**
 * A count with lakh-style grouping (the same grouping
 * formatBdt uses for taka), so big dashboard numbers stay
 * readable: 1234567 → "12,34,567".
 */
export function formatCount(n: number): string {
  return n.toLocaleString("en-IN");
}

/**
 * A byte count in the largest unit that keeps the value
 * at or above 1 (B / KB / MB / GB), one decimal place
 * past bytes. Added for the admin overview's media block:
 * the page used to print raw KB unconditionally, which
 * turns unreadable at real catalogue scale.
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0
    ? `${Math.round(value)} ${units[unit]}`
    : `${value.toFixed(1)} ${units[unit]}`;
}

/**
 * An age in seconds as a short Bangla duration — the
 * largest whole unit (দিন / ঘণ্টা / মিনিট), "১ মিনিটের কম"
 * under a minute. For queue ages on ops surfaces, where
 * "how long has this waited" is the whole point.
 */
export function formatAgeSeconds(secs: number | null | undefined): string {
  if (secs === null || secs === undefined || !Number.isFinite(secs)) {
    return "—";
  }
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return "১ মিনিটের কম";
  const minutes = Math.floor(s / 60);
  if (minutes < 60) return `${minutes} মিনিট`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} ঘণ্টা`;
  const days = Math.floor(hours / 24);
  return `${days} দিন`;
}

/**
 * A timestamp pinned to Asia/Dhaka, for ops surfaces: the
 * platform's day boundaries are Dhaka days, so an admin
 * abroad should still read Dhaka wall-clock here.
 */
export function formatDhakaDateTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleString("en-GB", {
    timeZone: "Asia/Dhaka",
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}
