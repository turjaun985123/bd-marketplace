"use client";

import { useCallback, useEffect, useState } from "react";
import AdminShell from "@/components/AdminShell";
import { getMe, listAdminUsers, setUserAdmin, type AdminUser } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Admin users (Wave 10): everyone on the platform, searchable by
 * phone, with the admin-flag toggle. The toggle never appears for
 * your own row — the API refuses self-changes with a 400 anyway,
 * so the UI does not offer a button that can only fail.
 */
export default function AdminUsersPage() {
  return (
    <AdminShell>
      <Users />
    </AdminShell>
  );
}

function Users() {
  const { token } = useAuth();
  const [users, setUsers] = useState<AdminUser[]>([]);
  const [myId, setMyId] = useState<string | null>(null);
  const [q, setQ] = useState("");
  const [message, setMessage] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const [list, me] = await Promise.all([
        listAdminUsers(token, { q: q.trim() || undefined, limit: 50 }),
        getMe(token),
      ]);
      setUsers(list.items);
      setMyId(me.id);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "ব্যবহারকারী লোড করা যায়নি।"
      );
    }
  }, [token, q]);

  useEffect(() => {
    void load();
  }, [load]);

  async function toggle(user: AdminUser) {
    if (!token) return;
    setBusyId(user.id);
    setMessage(null);
    try {
      await setUserAdmin(token, user.id, !user.is_admin);
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "ফ্ল্যাগ বদলানো যায়নি।");
    } finally {
      setBusyId(null);
    }
  }

  return (
    <>
      <h2>ব্যবহারকারী — Users</h2>
      <label>
        খুঁজুন (ফোন নম্বর)
        <input value={q} onChange={(e) => setQ(e.target.value)} />
      </label>
      {message && <p>{message}</p>}
      <table>
        <thead>
          <tr>
            <th>ফোন</th>
            <th>নাম</th>
            <th>ভূমিকা</th>
            <th>স্টোর</th>
            <th>অ্যাডমিন</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {users.map((user) => (
            <tr key={user.id}>
              <td>{user.phone}</td>
              <td>{user.name ?? "—"}</td>
              <td>{user.role}</td>
              <td>{user.store_count}</td>
              <td>{user.is_admin ? "হ্যাঁ" : "না"}</td>
              <td>
                {user.id !== myId && (
                  <button
                    type="button"
                    disabled={busyId === user.id}
                    onClick={() => void toggle(user)}
                  >
                    {user.is_admin ? "অ্যাডমিন সরান" : "অ্যাডমিন করুন"}
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {users.length === 0 && <p>কোনো ব্যবহারকারী পাওয়া যায়নি।</p>}
    </>
  );
}
