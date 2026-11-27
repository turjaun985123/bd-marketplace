"use client";

import { useEffect, useState } from "react";
import {
  getCategoryTree,
  type CategoryTreeNode,
} from "@/lib/api";
import CategoryIcon from "./CategoryIcon";

/**
 * The seller's category picker (Wave 56): a tree select
 * fed by the public taxonomy (GET /v1/categories/tree),
 * replacing the free-text category input the product form
 * used to carry. Options are the tree flattened in display
 * order with the full breadcrumb as the label, so a seller
 * sees exactly where a product will be filed ("Fashion ›
 * Men's Fashion › Panjabi").
 *
 * The value model mirrors the API's resolution rule:
 * - a chosen node sends `category_id` (the API syncs the
 *   product's label to the node's English name);
 * - "নিজের লেবেল — custom label" sends only the typed
 *   `category` text (the API links it if it exactly
 *   matches a node, else the product stays free-text);
 * - nothing chosen sends neither.
 *
 * There is deliberately no "force a fit" behaviour: the
 * seed tree's Others (অন্যান্য) node is the catch-all, and
 * the custom-label escape hatch stays for labels the
 * taxonomy genuinely does not cover yet.
 */
export interface CategoryPick {
  category_id: string | null;
  category: string;
}

/** The picker's empty value (nothing chosen). */
export const EMPTY_CATEGORY_PICK: CategoryPick = {
  category_id: null,
  category: "",
};

const CUSTOM_VALUE = "__custom__";

/** One flattened tree row for select rendering. */
export interface FlatCategoryOption {
  id: string;
  slug: string;
  /** Breadcrumb label, e.g. "Fashion › Men's Fashion › Panjabi". */
  label: string;
  name_bn: string | null;
  icon_key: string;
  depth: number;
}

/** Flatten a category tree in display order, breadcrumbs as labels. */
export function flattenCategoryTree(
  nodes: CategoryTreeNode[],
  trail: string[] = []
): FlatCategoryOption[] {
  const out: FlatCategoryOption[] = [];
  for (const node of nodes) {
    const label = [...trail, node.name_en].join(" › ");
    out.push({
      id: node.id,
      slug: node.slug,
      label,
      name_bn: node.name_bn,
      icon_key: node.icon_key,
      depth: node.depth,
    });
    out.push(...flattenCategoryTree(node.children, [...trail, node.name_en]));
  }
  return out;
}

/** Every id in a node's subtree (itself included), from a built tree. */
export function subtreeIdSet(
  nodes: CategoryTreeNode[],
  id: string
): Set<string> | null {
  for (const node of nodes) {
    if (node.id === id) {
      const ids = new Set<string>();
      const collect = (n: CategoryTreeNode) => {
        ids.add(n.id);
        n.children.forEach(collect);
      };
      collect(node);
      return ids;
    }
    const found = subtreeIdSet(node.children, id);
    if (found) return found;
  }
  return null;
}

export default function CategoryPicker({
  value,
  onChange,
}: {
  value: CategoryPick;
  onChange: (pick: CategoryPick) => void;
}) {
  const [options, setOptions] = useState<FlatCategoryOption[] | null>(null);
  const [custom, setCustom] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getCategoryTree()
      .then((result) => {
        if (!cancelled) setOptions(flattenCategoryTree(result.tree));
      })
      .catch(() => {
        if (!cancelled) setOptions([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const selectValue = custom
    ? CUSTOM_VALUE
    : (value.category_id ?? "");

  function onSelect(next: string) {
    if (next === CUSTOM_VALUE) {
      setCustom(true);
      onChange({ category_id: null, category: value.category });
    } else if (next === "") {
      setCustom(false);
      onChange({ category_id: null, category: "" });
    } else {
      setCustom(false);
      onChange({ category_id: next, category: "" });
    }
  }

  return (
    <>
      <select
        value={selectValue}
        onChange={(e) => onSelect(e.target.value)}
        disabled={options === null}
        aria-label="ক্যাটাগরি — Category"
      >
        <option value="">
          {options === null
            ? "ক্যাটাগরি লোড হচ্ছে…"
            : "— ক্যাটাগরি বেছে নিন — Choose a category —"}
        </option>
        {(options ?? []).map((option) => (
          <option key={option.id} value={option.id}>
            {"  ".repeat(Math.max(0, option.depth - 1))}
            {option.label}
            {option.name_bn ? ` (${option.name_bn})` : ""}
          </option>
        ))}
        <option value={CUSTOM_VALUE}>
          নিজের লেবেল লিখব — Use a custom label
        </option>
      </select>
      {custom && (
        <input
          value={value.category}
          onChange={(e) =>
            onChange({ category_id: null, category: e.target.value })
          }
          placeholder="যেমন: clothing"
          maxLength={60}
          aria-label="নিজের ক্যাটাগরি লেবেল — Custom category label"
        />
      )}
      {!custom && value.category_id && (
        <span className="category-picker-note">
          <CategoryIcon
            iconKey={
              options?.find((option) => option.id === value.category_id)
                ?.icon_key ?? "tag"
            }
          />{" "}
          পণ্যটি এই ক্যাটাগরিতে থাকবে — filed under this category
        </span>
      )}
    </>
  );
}
