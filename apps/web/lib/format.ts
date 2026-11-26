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
