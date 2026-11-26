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
