import { formatBdt } from "@/lib/format";

/** Shared price display. Amount is in poisha, as returned by the Rust API. */
export default function PriceTag({ amountPoisha }: { amountPoisha: number }) {
  return <span className="price">{formatBdt(amountPoisha)}</span>;
}
