"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import MessageThread from "@/components/MessageThread";
import {
  dismissSidraSuggestion,
  getSidraStatus,
  listConversations,
  listSidraSuggestions,
  sendSidraSuggestion,
  setSidraAutoReply,
  type Conversation,
  type SidraStatus,
  type SidraSuggestion,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The seller inbox (Wave 13): the store's conversations (the
 * shared MessageThread renders the selected one), the SIDRA
 * suggestion queue, and the SIDRA settings card.
 *
 * Suggestions are SIDRA's AI replies for a store that has NOT
 * enabled auto-reply: nothing reaches the buyer until the seller
 * presses পাঠান (send) here — and what is sent is still labelled
 * AI in the thread, because the words are SIDRA's.
 */
export default function SellerInboxPage() {
  const { ready, token } = useAuth();
  const [conversations, setConversations] = useState<Conversation[] | null>(
    null
  );
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [suggestions, setSuggestions] = useState<SidraSuggestion[] | null>(
    null
  );
  const [sidraStatus, setSidraStatus] = useState<SidraStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const reloadLists = useCallback(async () => {
    if (!token) return;
    const [list, pending, status] = await Promise.all([
      listConversations(token),
      listSidraSuggestions(token),
      getSidraStatus(token),
    ]);
    setConversations(list.filter((c) => c.role === "seller"));
    setSuggestions(pending);
    setSidraStatus(status);
  }, [token]);

  useEffect(() => {
    if (!ready || !token) return;
    let cancelled = false;
    reloadLists().catch((err) => {
      if (!cancelled) {
        setError(err instanceof Error ? err.message : "ইনবক্স লোড করা যায়নি।");
      }
    });
    return () => {
      cancelled = true;
    };
  }, [ready, token, reloadLists]);

  async function onDecide(suggestion: SidraSuggestion, send: boolean) {
    if (!token) return;
    setBusyId(suggestion.id);
    setError(null);
    try {
      if (send) {
        await sendSidraSuggestion(token, suggestion.id);
      } else {
        await dismissSidraSuggestion(token, suggestion.id);
      }
      await reloadLists();
    } catch (err) {
      setError(err instanceof Error ? err.message : "সিদ্ধান্ত পাঠানো যায়নি।");
    } finally {
      setBusyId(null);
    }
  }

  async function onToggleAutoReply(autoReply: boolean) {
    if (!token) return;
    setError(null);
    try {
      await setSidraAutoReply(token, autoReply);
      setSidraStatus((prev) =>
        prev ? { ...prev, auto_reply: autoReply } : prev
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : "সেটিং বদলানো যায়নি।");
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
        <h1>ইনবক্স — Inbox</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <p>
        <Link href="/seller">← বিক্রেতা ড্যাশবোর্ড</Link>
      </p>
      <h1>ইনবক্স — Inbox</h1>
      {error && <p role="alert">{error}</p>}

      <section>
        <h2>SIDRA সেটিং — AI salesperson</h2>
        {sidraStatus === null ? (
          <p>লোড হচ্ছে…</p>
        ) : (
          <div className="card">
            <p>
              প্ল্যাটফর্ম SIDRA ব্রিজ:{" "}
              <strong>
                {sidraStatus.configured
                  ? "চালু আছে (configured)"
                  : "চালু নেই (not configured)"}
              </strong>
            </p>
            {!sidraStatus.configured && (
              <p>
                SIDRA এখনো প্ল্যাটফর্মে যুক্ত হয়নি — ততক্ষণ ইনবক্স শুধু
                আপনার নিজের উত্তরেই চলবে।
              </p>
            )}
            <p>
              <label>
                <input
                  type="checkbox"
                  checked={sidraStatus.auto_reply}
                  onChange={(e) => void onToggleAutoReply(e.target.checked)}
                />{" "}
                অটো-রিপ্লাই — SIDRA-র উত্তর সরাসরি কথোপকথনে যাবে (বন্ধ
                থাকলে প্রতিটি উত্তর আগে আপনার অনুমোদন লাগবে)
              </label>
            </p>
          </div>
        )}
      </section>

      <section>
        <h2>SIDRA-র প্রস্তাবিত উত্তর — Suggestions</h2>
        {suggestions === null ? (
          <p>লোড হচ্ছে…</p>
        ) : suggestions.length === 0 ? (
          <p>কোনো অপেক্ষমাণ প্রস্তাব নেই।</p>
        ) : (
          suggestions.map((suggestion) => (
            <article key={suggestion.id} className="card">
              <p>
                <strong>{suggestion.buyer_display}</strong>{" "}
                <small>
                  {new Date(suggestion.created_at).toLocaleString("en-GB")}
                </small>
              </p>
              <p>{suggestion.body}</p>
              <p>
                <button
                  type="button"
                  disabled={busyId === suggestion.id}
                  onClick={() => void onDecide(suggestion, true)}
                >
                  পাঠান — Send
                </button>{" "}
                <button
                  type="button"
                  disabled={busyId === suggestion.id}
                  onClick={() => void onDecide(suggestion, false)}
                >
                  বাদ দিন — Dismiss
                </button>{" "}
                <Link href={`/seller/inbox#${suggestion.conversation_id}`}>
                  কথোপকথন দেখুন
                </Link>
              </p>
            </article>
          ))
        )}
      </section>

      <section>
        <h2>কথোপকথন — Conversations</h2>
        {conversations === null ? (
          <p>লোড হচ্ছে…</p>
        ) : conversations.length === 0 ? (
          <p>এখনো কোনো ক্রেতা মেসেজ করেননি।</p>
        ) : (
          conversations.map((conversation) => (
            <article key={conversation.id} className="card">
              <p>
                <button
                  type="button"
                  onClick={() => setSelectedId(conversation.id)}
                >
                  <strong>{conversation.buyer_display}</strong>
                </button>{" "}
                {conversation.unread_count > 0 && (
                  <span>({conversation.unread_count}টি নতুন)</span>
                )}{" "}
                {conversation.status === "closed" && <span>· বন্ধ</span>}
              </p>
              {conversation.product_title && (
                <p>পণ্য: {conversation.product_title}</p>
              )}
              <p>
                {conversation.last_message_preview ?? "এখনো কোনো মেসেজ নেই"}
              </p>
            </article>
          ))
        )}
      </section>

      {selectedId && (
        <section>
          <h2>থ্রেড — Thread</h2>
          <MessageThread
            key={selectedId}
            token={token}
            conversationId={selectedId}
            onConversationChange={() => void reloadLists()}
          />
        </section>
      )}
    </main>
  );
}
