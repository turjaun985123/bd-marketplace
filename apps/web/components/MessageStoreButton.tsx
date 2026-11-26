"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { createConversation, getStore } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * "মেসেজ করুন" for a storefront (Wave 13): resolves the store by
 * slug, finds-or-creates the caller's conversation with it via
 * the one API endpoint, and routes to the thread. Signed-out
 * visitors get a login link instead of a button.
 */
export default function MessageStoreButton({ slug }: { slug: string }) {
  const { token } = useAuth();
  const router = useRouter();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!token) {
    return (
      <p>
        বিক্রেতাকে মেসেজ করতে <Link href="/login">লগইন করুন</Link>।
      </p>
    );
  }

  async function onClick() {
    if (!token) return;
    setBusy(true);
    setError(null);
    try {
      const store = await getStore(slug);
      const conversation = await createConversation(token, store.id);
      router.push(`/messages/${conversation.id}`);
    } catch (err) {
      setError(err instanceof Error ? err.message : "মেসেজ শুরু করা যায়নি।");
      setBusy(false);
    }
  }

  return (
    <p>
      <button type="button" disabled={busy} onClick={() => void onClick()}>
        মেসেজ করুন — Message the seller
      </button>
      {error && <span role="alert"> {error}</span>}
    </p>
  );
}
