"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { listConversations, type Conversation } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The signed-in user's conversation list (Wave 13): every thread
 * they take part in — as a buyer with stores — newest activity
 * first, with an unread badge per thread. The API computes the
 * counts from the read markers; this page only renders them.
 */
export default function MessagesPage() {
  const { ready, token } = useAuth();
  const [conversations, setConversations] = useState<Conversation[] | null>(
    null
  );
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!ready || !token) return;
    let cancelled = false;
    listConversations(token)
      .then((list) => {
        if (!cancelled) {
          setConversations(list.filter((c) => c.role === "buyer"));
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setError(
            err instanceof Error ? err.message : "মেসেজ লোড করা যায়নি।"
          );
        }
      });
    return () => {
      cancelled = true;
    };
  }, [ready, token]);

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
      <h1>মেসেজ — Messages</h1>
      {error && <p role="alert">{error}</p>}
      {conversations === null && !error && <p>লোড হচ্ছে…</p>}
      {conversations !== null && conversations.length === 0 && (
        <p>
          এখনো কোনো কথোপকথন নেই — কোনো স্টোরের পেজ বা পণ্যের পেজ থেকে
          “মেসেজ করুন” চাপলেই শুরু হবে।
        </p>
      )}
      {conversations?.map((conversation) => (
        <article key={conversation.id} className="card">
          <p>
            <Link href={`/messages/${conversation.id}`}>
              <strong>{conversation.store.name}</strong>
            </Link>{" "}
            {conversation.unread_count > 0 && (
              <span>({conversation.unread_count}টি নতুন)</span>
            )}{" "}
            {conversation.status === "closed" && <span>· বন্ধ</span>}
          </p>
          {conversation.product_title && (
            <p>পণ্য: {conversation.product_title}</p>
          )}
          <p>{conversation.last_message_preview ?? "এখনো কোনো মেসেজ নেই"}</p>
          <p>
            <small>
              {new Date(conversation.last_message_at).toLocaleString("en-GB")}
            </small>
          </p>
        </article>
      ))}
    </main>
  );
}
