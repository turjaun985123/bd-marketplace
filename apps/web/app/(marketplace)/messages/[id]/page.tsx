"use client";

import Link from "next/link";
import MessageThread from "@/components/MessageThread";
import { useAuth } from "@/hooks/useAuth";

/**
 * One buyer conversation (Wave 13): a thin page around the shared
 * MessageThread component — the same component renders the
 * seller's side in the seller inbox.
 */
export default function MessageThreadPage({
  params,
}: {
  params: { id: string };
}) {
  const { ready, token } = useAuth();

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
        <h1>মেসেজ — Messages</h1>
        <p>
          মেসেজ দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <p>
        <Link href="/messages">← সব মেসেজ</Link>
      </p>
      <h1>কথোপকথন — Conversation</h1>
      <MessageThread token={token} conversationId={params.id} />
    </main>
  );
}
