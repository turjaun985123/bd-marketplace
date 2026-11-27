"use client";

import Link from "next/link";
import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import CategoryIcon from "@/components/CategoryIcon";
import { Badge, Button, EmptyState } from "@/components/ui";
import { useAuth } from "@/hooks/useAuth";
import {
  getAdminCategoryTree,
  reorderCategories,
  updateCategory,
  type CategoryTreeNode,
} from "@/lib/api";

/**
 * The taxonomy TREE editor (Wave 56): the companion to
 * the flat admin list at /admin/categories. Restructuring
 * a tree is awkward in a list, so this page renders the
 * actual tree (GET /v1/admin/categories/tree — inactive
 * nodes included, flagged) with the structural moves as
 * buttons on every node:
 *
 * - ↑ / ↓ swap the node's sort order with its sibling
 *   (the bulk reorder endpoint, one transaction);
 * - ← promotes the node one level (reparent to its
 *   grandparent, or to the root at depth 2);
 * - → demotes the node under its previous sibling (the
 *   only demotion that never surprises: the sibling
 *   directly above becomes the parent);
 * - সক্রিয়/বন্ধ toggles is_active.
 *
 * Every move is a normal audited PATCH/reorder on the
 * server; the domain guards (cycle, depth cap) answer
 * with their bilingual errors, shown verbatim. After any
 * write the tree reloads from the server — the page
 * keeps no local copy of the structure to drift.
 */
export default function AdminCategoryTreePage() {
  return (
    <AdminShell>
      <CategoryTreeEditor />
    </AdminShell>
  );
}

/** One row of the flattened, expand-aware tree render. */
interface TreeRow {
  node: CategoryTreeNode;
  siblings: CategoryTreeNode[];
  index: number;
}

function CategoryTreeEditor() {
  const { token } = useAuth();
  const [tree, setTree] = useState<CategoryTreeNode[] | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const body = await getAdminCategoryTree(token);
      setTree(body.tree);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  /** Run one structural write, then reload the tree. */
  const act = useCallback(
    async (label: string, write: () => Promise<unknown>) => {
      if (busy) return;
      setBusy(true);
      setError(null);
      setNotice(null);
      try {
        await write();
        setNotice(`${label} — সম্পন্ন হয়েছে (done).`);
        await load();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setBusy(false);
      }
    },
    [busy, load]
  );

  if (!token) return <p>লগইন করা হয়নি — Not signed in.</p>;
  if (tree === null) return <p>লোড হচ্ছে… (Loading…)</p>;

  const rows: TreeRow[] = [];
  const walk = (nodes: CategoryTreeNode[]) => {
    nodes.forEach((node, index) => {
      rows.push({ node, siblings: nodes, index });
      if (!collapsed.has(node.id)) walk(node.children);
    });
  };
  walk(tree);

  const toggleCollapsed = (id: string) =>
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const moveSibling = (row: TreeRow, direction: -1 | 1) => {
    const other = row.siblings[row.index + direction];
    if (!other) return;
    void act(`"${row.node.name_en}" সরানো`, () =>
      reorderCategories(token, [
        { id: row.node.id, sort_order: other.sort_order },
        { id: other.id, sort_order: row.node.sort_order },
      ])
    );
  };

  const promote = (row: TreeRow) => {
    // The parent becomes the grandparent (null at depth
    // 2 → the node becomes a root).
    void act(`"${row.node.name_en}" উপরে তোলা`, () =>
      updateCategory(token, row.node.id, {
        parent_id: grandparentId(tree, row.node),
      })
    );
  };

  const demote = (row: TreeRow) => {
    const newParent = row.siblings[row.index - 1];
    if (!newParent) return;
    void act(`"${row.node.name_en}" ভিতরে নেওয়া`, () =>
      updateCategory(token, row.node.id, { parent_id: newParent.id })
    );
  };

  return (
    <section>
      <h1>ক্যাটাগরি ট্রি — Category tree</h1>
      <p>
        গাছের কাঠামো এখানে বদলান — Restructure the tree here; names,
        icons and the rest live on the{" "}
        <Link href="/admin/categories">তালিকা পেজে — list page</Link>.
        Moves are audited and the depth cap (3) applies.
      </p>
      {error && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}
      {notice && <p>{notice}</p>}
      {rows.length === 0 ? (
        <EmptyState title="কোনো ক্যাটাগরি নেই — No categories">
          Seed বা import করা ক্যাটাগরি এখানে দেখা যাবে।
        </EmptyState>
      ) : (
        <ul className="admin-tree">
          {rows.map((row) => {
            const { node } = row;
            const hasChildren = node.children.length > 0;
            const isCollapsed = collapsed.has(node.id);
            return (
              <li
                key={node.id}
                className="admin-tree-row"
                style={{ marginLeft: `${(node.depth - 1) * 28}px` }}
              >
                {hasChildren ? (
                  <button
                    type="button"
                    className="admin-tree-toggle"
                    aria-expanded={!isCollapsed}
                    aria-label={isCollapsed ? "খুলুন — expand" : "বন্ধ করুন — collapse"}
                    onClick={() => toggleCollapsed(node.id)}
                  >
                    {isCollapsed ? "▸" : "▾"}
                  </button>
                ) : (
                  <span className="admin-tree-toggle-spacer" />
                )}
                <CategoryIcon iconKey={node.icon_key} />{" "}
                <strong>{node.name_bn ?? node.name_en}</strong>{" "}
                <span className="category-landing-en">{node.name_en}</span>{" "}
                <Badge tone={node.is_active ? "brand" : "muted"}>
                  {node.is_active ? "সক্রিয়" : "বন্ধ"}
                </Badge>{" "}
                {node.is_featured && <Badge tone="accent">ফিচার্ড</Badge>}{" "}
                <span className="store-category-path">
                  {node.product_count}টি পণ্য (উপ-সহ) — products incl.
                  sub-categories
                </span>{" "}
                <span className="admin-tree-actions">
                  <Button
                    size="sm"
                    disabled={busy || row.index === 0}
                    onClick={() => moveSibling(row, -1)}
                  >
                    ↑
                  </Button>{" "}
                  <Button
                    size="sm"
                    disabled={busy || row.index === row.siblings.length - 1}
                    onClick={() => moveSibling(row, 1)}
                  >
                    ↓
                  </Button>{" "}
                  <Button
                    size="sm"
                    disabled={busy || node.depth === 1}
                    onClick={() => promote(row)}
                  >
                    ← উপরে
                  </Button>{" "}
                  <Button
                    size="sm"
                    disabled={busy || row.index === 0}
                    onClick={() => demote(row)}
                  >
                    → ভিতরে
                  </Button>{" "}
                  <Button
                    size="sm"
                    disabled={busy}
                    onClick={() =>
                      void act(`"${node.name_en}" অবস্থা বদল`, () =>
                        updateCategory(token, node.id, {
                          is_active: !node.is_active,
                        })
                      )
                    }
                  >
                    {node.is_active ? "বন্ধ করুন" : "সক্রিয় করুন"}
                  </Button>
                </span>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

/**
 * The id of a node's grandparent within the loaded
 * tree, or null when the node is at depth 2 (promoting
 * it makes it a root) — depth 1 never promotes (the
 * button is disabled there).
 */
function grandparentId(
  tree: CategoryTreeNode[],
  target: CategoryTreeNode
): string | null {
  for (const root of tree) {
    for (const child of root.children) {
      if (child.id === target.id) return null;
      for (const grandchild of child.children) {
        if (grandchild.id === target.id) return root.id;
      }
    }
  }
  return null;
}
