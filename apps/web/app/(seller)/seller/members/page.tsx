"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import StoreSwitcher from "@/components/StoreSwitcher";
import {
  addStoreMember,
  listSellerStores,
  listStoreMembers,
  removeStoreMember,
  storeRoleLabel,
  updateStoreMemberRole,
  type SellerStore,
  type StoreMember,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Store team page (Wave 34): who serves this store besides its
 * owner. The owner seats EXISTING accounts by phone number as
 * manager or staff, changes roles, and removes members; a
 * manager sees the same list read-only; staff hold no team
 * rights at all (the API answers 403 and the dashboard hides
 * this page's link for them).
 *
 * What each role may do is decided by the API's permission
 * matrix, never here: manager = everything except seating
 * members and payouts; staff = catalogue, orders and Q&A
 * only. This page only renders what the API returns and
 * surfaces its (bilingual) refusals verbatim.
 */
export default function SellerMembersPage() {
  const { ready, token } = useAuth();
  const [stores, setStores] = useState<SellerStore[]>([]);
  const [storeId, setStoreId] = useState("");
  const [members, setMembers] = useState<StoreMember[] | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  // Add-member form state (owner only).
  const [phone, setPhone] = useState("");
  const [newRole, setNewRole] = useState("staff");
  const [busy, setBusy] = useState(false);

  const myStore = stores.find((store) => store.id === storeId);
  const myRole = myStore?.my_role;
  const isOwner = myRole === "owner";

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    (async () => {
      try {
        const list = await listSellerStores(token);
        if (cancelled) return;
        setStores(list);
        const saved = window.localStorage.getItem("seller-store-id");
        const initial =
          saved && list.some((store) => store.id === saved)
            ? saved
            : (list[0]?.id ?? "");
        setStoreId(initial);
      } catch (err) {
        if (!cancelled) {
          setMessage(
            err instanceof Error ? err.message : "স্টোর লোড করা যায়নি।"
          );
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [token]);

  const loadMembers = useCallback(async () => {
    if (!token || !storeId) return;
    try {
      setMembers(await listStoreMembers(token, storeId));
    } catch (err) {
      setMembers(null);
      setMessage(err instanceof Error ? err.message : "টিম লোড করা যায়নি।");
    }
  }, [token, storeId]);

  useEffect(() => {
    void loadMembers();
  }, [loadMembers]);

  function onSelectStore(next: string) {
    setStoreId(next);
    setMembers(null);
    setMessage(null);
    if (next) window.localStorage.setItem("seller-store-id", next);
  }

  async function onAdd(e: React.FormEvent) {
    e.preventDefault();
    if (!token || !storeId) return;
    setBusy(true);
    setMessage(null);
    try {
      await addStoreMember(token, storeId, phone.trim(), newRole);
      setPhone("");
      setMessage("সদস্য যোগ হয়েছে। (Member added.)");
      await loadMembers();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "যোগ করা যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  async function onChangeRole(member: StoreMember, role: string) {
    if (!token || !storeId) return;
    setMessage(null);
    try {
      await updateStoreMemberRole(token, storeId, member.user_id, role);
      await loadMembers();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "ভূমিকা বদলানো যায়নি।");
    }
  }

  async function onRemove(member: StoreMember) {
    if (!token || !storeId) return;
    setMessage(null);
    try {
      await removeStoreMember(token, storeId, member.user_id);
      setMessage("সদস্য সরানো হয়েছে। (Member removed.)");
      await loadMembers();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "সরানো যায়নি।");
    }
  }

  if (!ready) {
    return (
      <main>
        <p>লোড হচ্ছে…</p>
      </main>
    );
  }

  if (!token) {
    return (
      <main>
        <h1>স্টোরের টিম</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>
        স্টোরের টিম — Store team
        {myStore ? ` — ${myStore.name}` : ""}
        {myRole ? ` (${storeRoleLabel(myRole)})` : ""}
      </h1>
      <p>
        <Link href="/seller">ড্যাশবোর্ডে ফিরুন — Back to dashboard</Link>
      </p>
      <StoreSwitcher
        stores={stores}
        value={storeId}
        onChange={onSelectStore}
        includeAll={false}
      />
      {message && <p role="status">{message}</p>}

      {stores.length === 0 ? (
        <p>আপনার কোনো স্টোর নেই। (You have no store yet.)</p>
      ) : members === null ? (
        <p>লোড হচ্ছে…</p>
      ) : (
        <>
          {members.length === 0 ? (
            <p>
              এই স্টোরে এখনো কোনো টিম সদস্য নেই — শুধু মালিকই কাজ
              করছেন। (No team members yet.)
            </p>
          ) : (
            members.map((member) => (
              <article key={member.user_id} className="card">
                <p>
                  {member.phone_masked} —{" "}
                  <strong>{storeRoleLabel(member.role)}</strong> (
                  {member.role}) · যোগ দিয়েছেন{" "}
                  {new Date(member.created_at).toLocaleString("bn-BD")}
                </p>
                {isOwner && (
                  <p>
                    <label>
                      ভূমিকা (role){" "}
                      <select
                        value={member.role}
                        onChange={(e) =>
                          void onChangeRole(member, e.target.value)
                        }
                      >
                        <option value="manager">ম্যানেজার — Manager</option>
                        <option value="staff">স্টাফ — Staff</option>
                      </select>
                    </label>{" "}
                    <button
                      type="button"
                      onClick={() => void onRemove(member)}
                    >
                      সরিয়ে দিন — Remove
                    </button>
                  </p>
                )}
              </article>
            ))
          )}

          {isOwner ? (
            <section>
              <h2>নতুন সদস্য যোগ করুন — Add a member</h2>
              <p>
                যাকে যোগ করবেন তার এই প্ল্যাটফর্মে আগে থেকেই একটি
                অ্যাকাউন্ট থাকতে হবে — মোবাইল নম্বর দিয়ে অ্যাকাউন্ট
                খুঁজে সদস্য করা হয়, নতুন অ্যাকাউন্ট তৈরি হয় না।
                ম্যানেজার পে-আউট আর সদস্য ব্যবস্থাপনা ছাড়া সব করতে
                পারেন; স্টাফ শুধু পণ্য, অর্ডার আর প্রশ্নোত্তর সামলান।
              </p>
              <form onSubmit={(e) => void onAdd(e)}>
                <label>
                  মোবাইল নম্বর (mobile number){" "}
                  <input
                    value={phone}
                    onChange={(e) => setPhone(e.target.value)}
                    placeholder="01712345678"
                    inputMode="tel"
                    required
                  />
                </label>{" "}
                <label>
                  ভূমিকা (role){" "}
                  <select
                    value={newRole}
                    onChange={(e) => setNewRole(e.target.value)}
                  >
                    <option value="staff">স্টাফ — Staff</option>
                    <option value="manager">ম্যানেজার — Manager</option>
                  </select>
                </label>{" "}
                <button type="submit" disabled={busy}>
                  {busy ? "যোগ হচ্ছে…" : "যোগ করুন"}
                </button>
              </form>
            </section>
          ) : (
            <p>
              সদস্য যোগ করা, ভূমিকা বদলানো আর সরানোর কাজ শুধু স্টোরের
              মালিক করেন। (Only the store owner manages the team.)
            </p>
          )}
        </>
      )}
    </main>
  );
}
