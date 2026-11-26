"use client";

import { useCallback, useEffect, useState, type FormEvent } from "react";
import Link from "next/link";
import {
  apiKeyStatusLabel,
  createApiKey,
  listApiKeys,
  revokeApiKey,
  type ApiKeyView,
  type CreatedApiKey,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * API keys (Wave 37): the account's long-lived keys for
 * external apps — a mobile app, an integration — managed
 * from the seller area.
 *
 * The security rules live on the API and are only surfaced
 * here: only the key's HASH is stored, so the full key
 * exists solely in the create response — this page keeps it
 * in the reveal panel until the seller dismisses it, and no
 * reload can bring it back (the list below carries prefixes
 * only). A key acts as its owner, narrowed by its scopes;
 * admin routes, payout and subscription money movement,
 * data exports and key management itself stay JWT-only on
 * the API whatever the key's scopes. Keys belong to the
 * ACCOUNT, not to one store — there is deliberately no
 * store switcher here.
 */
export default function SellerApiKeysPage() {
  const { ready, token } = useAuth();
  const [keys, setKeys] = useState<ApiKeyView[]>([]);
  const [message, setMessage] = useState<string | null>(null);
  const [revealed, setRevealed] = useState<CreatedApiKey | null>(null);
  const [copied, setCopied] = useState(false);
  const [revokingId, setRevokingId] = useState<string | null>(null);

  // Create form state.
  const [name, setName] = useState("");
  const [writeScope, setWriteScope] = useState(false);
  const [expiryDays, setExpiryDays] = useState("");
  const [creating, setCreating] = useState(false);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listApiKeys(token);
      setKeys(list.items);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "API কী লোড করা যায়নি।"
      );
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  async function onCreate(event: FormEvent) {
    event.preventDefault();
    if (!token) return;
    setCreating(true);
    setMessage(null);
    setRevealed(null);
    setCopied(false);
    try {
      const days = expiryDays.trim() === "" ? null : Number(expiryDays);
      const created = await createApiKey(
        token,
        name.trim(),
        writeScope ? ["read", "write"] : ["read"],
        days
      );
      setRevealed(created);
      setName("");
      setWriteScope(false);
      setExpiryDays("");
      void load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "API কী তৈরি করা যায়নি।");
    } finally {
      setCreating(false);
    }
  }

  async function onCopyKey() {
    if (!revealed) return;
    try {
      await navigator.clipboard.writeText(revealed.key);
      setCopied(true);
    } catch {
      setMessage(
        "কপি করা যায়নি — কীটি হাতে সিলেক্ট করে কপি করুন।"
      );
    }
  }

  async function onRevoke(key: ApiKeyView) {
    if (!token) return;
    const confirmed = window.confirm(
      `"${key.name}" কীটি বাতিল করবেন? যে অ্যাপ এই কী ব্যবহার করছে তা সঙ্গে সঙ্গে বন্ধ হয়ে যাবে।`
    );
    if (!confirmed) return;
    setRevokingId(key.id);
    setMessage(null);
    try {
      await revokeApiKey(token, key.id);
      setMessage(`"${key.name}" বাতিল হয়েছে।`);
      void load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "কী বাতিল করা যায়নি।");
    } finally {
      setRevokingId(null);
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
        <h1>API কী</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>API কী — API keys</h1>
      <p>
        <Link href="/seller">ড্যাশবোর্ডে ফিরুন — Back to dashboard</Link>
      </p>
      <p>
        API কী দিয়ে বাইরের অ্যাপ (যেমন মোবাইল অ্যাপ বা আপনার নিজের
        সফটওয়্যার) লগইন ছাড়াই আপনার অ্যাকাউন্টের হয়ে কাজ করতে পারে।
        কী ফাঁস হলে সঙ্গে সঙ্গে বাতিল করুন — বাতিল করা কী আর কখনো চলবে
        না। অ্যাডমিন, পে-আউট আর সাবস্ক্রিপশনের মতো টাকার কাজ API কী
        দিয়ে করা যায় না — সেগুলোর জন্য লগইন সেশনই লাগবে।
      </p>
      {message && <p role="status">{message}</p>}

      {revealed && (
        <section>
          <div className="card">
            <h2>আপনার নতুন API কী — Your new API key</h2>
            <p>
              <strong>
                এই কীটি আর দেখানো হবে না — এখনই কপি করুন।
              </strong>{" "}
              (This key will not be shown again — copy it now. পেজটি
              রিলোড করলে কীটি হারিয়ে যাবে; তখন নতুন কী বানাতে হবে।)
            </p>
            <p>
              <code>{revealed.key}</code>
            </p>
            <p>
              <button type="button" onClick={() => void onCopyKey()}>
                {copied ? "কপি হয়েছে ✓" : "কী কপি করুন — Copy key"}
              </button>{" "}
              <button type="button" onClick={() => setRevealed(null)}>
                বন্ধ করুন — Dismiss
              </button>
            </p>
          </div>
        </section>
      )}

      <section>
        <h2>নতুন কী বানান — Create a key</h2>
        <form onSubmit={(event) => void onCreate(event)}>
          <p>
            <label>
              কী-র নাম — Name{" "}
              <input
                type="text"
                value={name}
                onChange={(event) => setName(event.target.value)}
                placeholder="যেমন: Mobile app"
                maxLength={60}
                required
              />
            </label>
          </p>
          <p>
            <label>
              <input type="checkbox" checked readOnly /> শুধু দেখা — Read
              (তথ্য পড়তে পারবে)
            </label>{" "}
            <label>
              <input
                type="checkbox"
                checked={writeScope}
                onChange={(event) => setWriteScope(event.target.checked)}
              />{" "}
              পরিবর্তন — Write (অর্ডার, কার্ট, পণ্য বদলাতে পারবে)
            </label>
          </p>
          <p>
            <label>
              মেয়াদ (দিন) — Expires in days{" "}
              <input
                type="number"
                value={expiryDays}
                onChange={(event) => setExpiryDays(event.target.value)}
                placeholder="খালি রাখলে মেয়াদ নেই"
                min={1}
                max={365}
              />
            </label>
          </p>
          <button type="submit" disabled={creating}>
            {creating ? "বানানো হচ্ছে…" : "কী বানান — Create key"}
          </button>
        </form>
      </section>

      <section>
        <h2>আমার কীগুলো — My keys</h2>
        {keys.length === 0 ? (
          <p>এখনো কোনো API কী বানানো হয়নি।</p>
        ) : (
          keys.map((key) => (
            <article key={key.id} className="card">
              <h3>{key.name}</h3>
              <p>
                <code>{key.key_prefix}…</code> ·{" "}
                {key.scopes.includes("write")
                  ? "দেখা + পরিবর্তন — Read + write"
                  : "শুধু দেখা — Read only"}{" "}
                · {apiKeyStatusLabel(key.status)}
              </p>
              <p>
                তৈরি: {new Date(key.created_at).toLocaleString("bn-BD")} ·
                শেষ ব্যবহার:{" "}
                {key.last_used_at
                  ? new Date(key.last_used_at).toLocaleString("bn-BD")
                  : "এখনো হয়নি"}{" "}
                · মেয়াদ শেষ:{" "}
                {key.expires_at
                  ? new Date(key.expires_at).toLocaleDateString("bn-BD")
                  : "নেই"}
              </p>
              {key.status === "active" && (
                <button
                  type="button"
                  onClick={() => void onRevoke(key)}
                  disabled={revokingId === key.id}
                >
                  {revokingId === key.id ? "বাতিল হচ্ছে…" : "বাতিল করুন — Revoke"}
                </button>
              )}
            </article>
          ))
        )}
      </section>
    </main>
  );
}
