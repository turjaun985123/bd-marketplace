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
