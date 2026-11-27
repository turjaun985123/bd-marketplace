"use client";

import type { District } from "@/lib/api";

/**
 * District picker (Wave 55), shared by the address book
 * form and the checkout form so the two can never drift:
 * one option list (Bangla name, English name, zone), one
 * placeholder convention. The value is the district's
 * English name — the API's canonical form; the zone is
 * always DERIVED from the district (server-side too),
 * never picked separately for a district-known address.
 */

/** Zone wire value → Bangla label, for district options and address cards. */
export const DISTRICT_ZONE_LABELS: Record<string, string> = {
  inside_dhaka: "ঢাকার ভিতরে",
  dhaka_suburb: "ঢাকার উপশহর",
  outside_dhaka: "ঢাকার বাইরে",
};

export default function DistrictSelect({
  districts,
  value,
  onChange,
  required = false,
  disabled = false,
  optional = false,
}: {
  districts: District[];
  /** The selected district's English name; "" for none. */
  value: string;
  onChange: (nameEn: string) => void;
  required?: boolean;
  disabled?: boolean;
  /** Show the "(ঐচ্ছিক)" hint in the placeholder. */
  optional?: boolean;
}) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      required={required}
      disabled={disabled}
    >
      <option value="">
        — জেলা বেছে নিন{optional ? " (ঐচ্ছিক)" : ""} —
      </option>
      {districts.map((district) => (
        <option key={district.name_en} value={district.name_en}>
          {district.name_bn} ({district.name_en}) —{" "}
          {DISTRICT_ZONE_LABELS[district.zone] ?? district.zone}
        </option>
      ))}
    </select>
  );
}
