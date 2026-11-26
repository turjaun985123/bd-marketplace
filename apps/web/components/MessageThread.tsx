"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import {
  closeConversation,
  getConversation,
  sendMessage,
  senderTypeLabel,
  type Conversation,
  type Message,
} from "@/lib/api";

/**
 * The shared conversation thread (Wave 13), used by the buyer's
 * message page and the seller inbox alike — one implementation,
 * never forked per side. The API decides the caller's role and
 * sender type; this component only renders and posts.
 *
 * Refresh is simple polling: the thread reloads every 10 seconds
 * (and immediately after sending). There is no websocket — the
 * README's Wave 13 section names that limit plainly.
 */
export default function MessageThread({
  token,
  conversationId,
  onConversationChange,
}: {
  token: string;
  conversationId: string;
  /** Called after close, so a parent list can refresh itself. */
  onConversationChange?: (conversation: Conversation) => void;
}) {
  const [conversation, setConversation] = useState<Conversation | null>(null);
  const [messages, setMessages] = useState<Message[] | null>(null);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const draftRef = useRef("");
  draftRef.current = draft;

  const reload = useCallback(async () => {
    const thread = await getConversation(token, conversationId);
    setConversation(thread.conversation);
    setMessages(thread.messages);
  }, [token, conversationId]);

  useEffect(() => {
    let cancelled = false;
    reload().catch((err) => {
      if (!cancelled) {
        setError(err instanceof Error ? err.message : "থ্রেড লোড করা যায়নি।");
      }
    });
    // Poll for the other side's messages (and SIDRA's replies).
    const timer = setInterval(() => {
      reload().catch(() => {
        // A failed poll keeps the last good state; the next tick retries.
      });
    }, 10_000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [reload]);

  async function onSend(e: React.FormEvent) {
    e.preventDefault();
    const body = draftRef.current.trim();
    if (!body || sending) return;
    setSending(true);
    setError(null);
    try {
      await sendMessage(token, conversationId, body);
      setDraft("");
      await reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : "মেসেজ পাঠানো যায়নি।");
    } finally {
      setSending(false);
    }
  }

  async function onClose() {
    setError(null);
    try {
      const updated = await closeConversation(token, conversationId);
      setConversation(updated);
      onConversationChange?.(updated);
    } catch (err) {
      setError(err instanceof Error ? err.message : "বন্ধ করা যায়নি।");
    }
  }

  if (messages === null && error === null) {
    return <p>লোড হচ্ছে… (Loading…)</p>;
  }
  if (error && messages === null) {
    return <p role="alert">{error}</p>;
  }

  const closed = conversation?.status === "closed";
  const ownType = conversation?.role ?? "";

  return (
    <section>
      {conversation?.product_title && (
        <p>
          পণ্য সম্পর্কে: <strong>{conversation.product_title}</strong>
        </p>
      )}
      {closed && (
        <p>
          এই কথোপকথন বন্ধ করা হয়েছে — নতুন মেসেজ পাঠানো যাবে না। (This
          conversation is closed.)
        </p>
      )}
      {messages !== null && messages.length === 0 && (
        <p>এখনো কোনো মেসেজ নেই — প্রথম মেসেজটি লিখুন।</p>
      )}
      {messages?.map((message) => (
        <article key={message.id} className="card">
          <p>
            <strong>{senderTypeLabel(message.sender_type)}</strong>{" "}
            <small>
              {new Date(message.created_at).toLocaleString("en-GB")}
            </small>
            {message.sender_type === ownType && <span> · আপনি</span>}
          </p>
          <p>{message.body}</p>
        </article>
      ))}

      {!closed && (
        <form onSubmit={(e) => void onSend(e)}>
          <label htmlFor={`message-box-${conversationId}`}>মেসেজ লিখুন</label>
          <textarea
            id={`message-box-${conversationId}`}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            rows={3}
            cols={50}
            maxLength={2000}
          />
          <p>
            <button type="submit" disabled={sending || draft.trim() === ""}>
              পাঠান — Send
            </button>{" "}
            <button type="button" onClick={() => void onClose()}>
              কথোপকথন বন্ধ করুন — Close
            </button>
          </p>
        </form>
      )}
      {error && <p role="alert">{error}</p>}
    </section>
  );
}
