"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { listMyNotifications } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * The header notification bell (Wave 54 extension): a
 * link to the inbox with the server's unread count as
 * its badge. Signed-out visitors get no bell (there is
 * no inbox to count). The count is read once per mount —
 * it is a doorway indicator, not a live ticker; the
 * inbox page itself always reads fresh.
 */
export default function NotificationBell() {
  const { token } = useAuth();
  const [unread, setUnread] = useState(0);

  useEffect(() => {
    if (!token) {
      setUnread(0);
      return;
    }
    let cancelled = false;
    listMyNotifications(token, { limit: 1 })
      .then((list) => {
        if (!cancelled) setUnread(list.unread_count);
      })
      .catch(() => {
        if (!cancelled) setUnread(0);
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  if (!token) return null;

  return (
    <p className="notification-bell">
      <Link href="/notifications">
        নোটিফিকেশন — Notifications
        {unread > 0 && (
          <span className="notification-badge">
            {" "}
            ({unread}টি অপঠিত)
          </span>
        )}
      </Link>
    </p>
  );
}
