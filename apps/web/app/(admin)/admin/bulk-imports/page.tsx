"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import { listAdminBulkImports, type AdminBulkImportEntry } from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { useAuth } from "@/hooks/useAuth";
import { Badge, EmptyState } from "@/components/ui";

/**
 * Admin bulk-import activity (Wave 57): the operator's
 * cross-store view of the seller bulk-import history —
 * who bulk-loaded what, how much of it landed, and which
 * stores' sheets keep failing (a support signal: a store
 * with a wall of error-heavy runs needs help with the
 * format, and a sudden burst of creations is worth a
 * look). Read-only; the runs themselves live in the
 * sellers' own histories.
 */
export default function AdminBulkImportsPage() {
  return (
    <AdminShell>
      <BulkImportsView />
    </AdminShell>
  );
}

function BulkImportsView() {
  const { token } = useAuth();
  const [entries, setEntries] = useState<AdminBulkImportEntry[] | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      setEntries(await listAdminBulkImports(token, 100));
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "ইমপোর্ট তালিকা লোড করা যায়নি।"
      );
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  if (message) return <p role="alert">{message}</p>;
  if (entries === null) return <p>লোড হচ্ছে…</p>;

  const totalCreated = entries.reduce((sum, entry) => sum + entry.created_count, 0);
  const totalErrors = entries.reduce((sum, entry) => sum + entry.error_count, 0);

  return (
    <>
      <h2>বাল্ক ইমপোর্ট — Bulk import activity</h2>
      <p>
        সাম্প্রতিক {entries.length}টি রানে তৈরি হয়েছে {totalCreated}টি পণ্য,
        ত্রুটি {totalErrors}টি সারিতে।
      </p>
      {entries.length === 0 ? (
        <EmptyState title="এখনো কোনো বাল্ক ইমপোর্ট হয়নি">
          <p>বিক্রেতারা CSV ইমপোর্ট চালালে তা এখানে দেখা যাবে।</p>
        </EmptyState>
      ) : (
        <div style={{ overflowX: "auto" }}>
          <table>
            <thead>
              <tr>
                <th>সময়</th>
                <th>স্টোর</th>
                <th>কে করেছেন</th>
                <th>মোট সারি</th>
                <th>তৈরি</th>
                <th>আপডেট</th>
                <th>ত্রুটি</th>
                <th>ধরন</th>
              </tr>
            </thead>
            <tbody>
              {entries.map((entry) => (
                <tr key={entry.id}>
                  <td>{formatDateTime(entry.created_at)}</td>
                  <td>
                    {entry.store_name} ({entry.store_slug})
                  </td>
                  <td>{entry.actor_phone_masked}</td>
                  <td>{entry.total_rows}</td>
                  <td>{entry.created_count}</td>
                  <td>{entry.updated_count}</td>
                  <td>{entry.error_count}</td>
                  <td>
                    {entry.dry_run ? (
                      <Badge tone="muted">ড্রাই রান</Badge>
                    ) : (
                      <Badge tone="brand">আসল রান</Badge>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}
