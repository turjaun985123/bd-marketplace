"use client";

import { storeRoleLabel, type SellerStore } from "@/lib/api";

/**
 * The Wave 34 store switcher: shown only when the signed-in
 * seller serves more than one store (their own plus stores
 * they are a team member of). The parent page owns the
 * selection state and persists it to localStorage under
 * `seller-store-id`; this component is purely the picker —
 * one shared component so the dashboard and the members page
 * can never drift apart.
 *
 * The "নিজের সব স্টোর — All my stores" option (value "")
 * returns to the default whole-activity view where the page
 * supports one (the dashboard); pages that act on exactly
 * one store pass `includeAll={false}`.
 */
export default function StoreSwitcher({
  stores,
  value,
  onChange,
  includeAll = true,
}: {
  stores: SellerStore[];
  /** The selected store id, or "" for the default view. */
  value: string;
  onChange: (storeId: string) => void;
  includeAll?: boolean;
}) {
  if (stores.length <= 1) return null;
  return (
    <p>
      <label>
        স্টোর বেছে নিন — Store:{" "}
        <select value={value} onChange={(e) => onChange(e.target.value)}>
          {includeAll && <option value="">নিজের সব স্টোর — All my stores</option>}
          {stores.map((store) => (
            <option key={store.id} value={store.id}>
              {store.name} ({storeRoleLabel(store.my_role)})
            </option>
          ))}
        </select>
      </label>
    </p>
  );
}
