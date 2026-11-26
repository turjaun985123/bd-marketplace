"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  answerQuestion,
  hideQuestion,
  listSellerQuestions,
  unhideQuestion,
  type SellerQuestion,
} from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

type Filter = "false" | "true" | "all";

const FILTER_LABELS: Record<Filter, string> = {
  false: "উত্তর বাকি — Unanswered",
  true: "উত্তর দেওয়া — Answered",
  all: "সব — All",
};

/**
 * Seller Q&A inbox (Wave 25): questions buyers asked on this
 * seller's products, across all their stores, from
 * GET /v1/seller/questions. The default filter is the work
 * queue (unanswered). Answering posts the public answer —
 * answering an already-answered question CORRECTS it (the API
 * updates the same row). Hide/unhide is the moderation switch:
 * hidden questions leave the product page and the dashboard
 * count but stay here, marked, so a hide can be undone.
 */
export default function SellerQuestionsPage() {
  const { ready, token } = useAuth();
  const [filter, setFilter] = useState<Filter>("false");
  const [items, setItems] = useState<SellerQuestion[] | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const list = await listSellerQuestions(token, filter);
      setItems(list.items);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "প্রশ্ন লোড করা যায়নি।"
      );
    }
  }, [token, filter]);

  useEffect(() => {
    void load();
  }, [load]);

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
        <h1>প্রশ্নোত্তর ইনবক্স</h1>
        <p>
          এই পেজটি দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  return (
    <main>
      <h1>প্রশ্নোত্তর ইনবক্স — Q&amp;A inbox</h1>
      <p>
        <Link href="/seller">← বিক্রেতা ড্যাশবোর্ড</Link>
      </p>
      <p>
        {(Object.keys(FILTER_LABELS) as Filter[]).map((f) => (
          <span key={f}>
            <button
              type="button"
              disabled={filter === f}
              onClick={() => setFilter(f)}
            >
              {FILTER_LABELS[f]}
            </button>{" "}
          </span>
        ))}
      </p>
      {message && <p role="status">{message}</p>}
      {items === null ? (
        <p>লোড হচ্ছে…</p>
      ) : items.length === 0 ? (
        <p>এই তালিকায় কোনো প্রশ্ন নেই। (No questions in this list.)</p>
      ) : (
        items.map((item) => (
          <QuestionInboxItem
            key={item.id}
            item={item}
            token={token}
            onChanged={() => void load()}
            onError={setMessage}
          />
        ))
      )}
    </main>
  );
}

function QuestionInboxItem({
  item,
  token,
  onChanged,
  onError,
}: {
  item: SellerQuestion;
  token: string;
  onChanged: () => void;
  onError: (message: string | null) => void;
}) {
  const [answerText, setAnswerText] = useState(item.answer_body ?? "");
  const [busy, setBusy] = useState(false);

  async function onAnswer() {
    setBusy(true);
    onError(null);
    try {
      await answerQuestion(token, item.id, answerText.trim());
      onChanged();
    } catch (err) {
      onError(err instanceof Error ? err.message : "উত্তর পাঠানো যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  async function onToggleHidden() {
    setBusy(true);
    onError(null);
    try {
      if (item.is_hidden) {
        await unhideQuestion(token, item.id);
      } else {
        await hideQuestion(token, item.id);
      }
      onChanged();
    } catch (err) {
      onError(err instanceof Error ? err.message : "পরিবর্তন করা যায়নি।");
    } finally {
      setBusy(false);
    }
  }

  return (
    <article className="card">
      <p>
        <Link href={`/products/${item.product.id}`}>
          {item.product.title}
        </Link>{" "}
        <small>({item.product.store_slug})</small>
        {item.is_hidden && (
          <>
            {" "}
            <strong>লুকানো — Hidden</strong>
          </>
        )}
      </p>
      <p>
        <strong>প্রশ্ন:</strong> {item.body}
      </p>
      <p>
        <small>
          {item.asker_display} ·{" "}
          {new Date(item.created_at).toLocaleString("bn-BD")}
        </small>
      </p>
      {item.answer_body && (
        <div className="card">
          <p>
            <strong>বিক্রেতার উত্তর</strong>
          </p>
          <p>{item.answer_body}</p>
        </div>
      )}
      <label>
        {item.answer_body ? "উত্তর সংশোধন করুন" : "উত্তর লিখুন"}{" "}
        <textarea
          value={answerText}
          onChange={(e) => setAnswerText(e.target.value)}
          rows={2}
          cols={50}
          maxLength={1000}
        />
      </label>
      <p>
        <button
          type="button"
          disabled={busy || answerText.trim().length === 0}
          onClick={() => void onAnswer()}
        >
          {item.answer_body ? "উত্তর সংরক্ষণ করুন" : "উত্তর দিন"}
        </button>{" "}
        <button
          type="button"
          disabled={busy}
          onClick={() => void onToggleHidden()}
        >
          {item.is_hidden ? "আবার দেখান — Unhide" : "লুকান — Hide"}
        </button>
      </p>
    </article>
  );
}
