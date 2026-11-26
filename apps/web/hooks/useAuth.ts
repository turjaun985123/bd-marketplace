"use client";

import { useCallback, useEffect, useState } from "react";
import type { User } from "@/lib/api";

const TOKEN_KEY = "bdmp_token";
const USER_KEY = "bdmp_user";

export interface AuthState {
  /** Null until the localStorage read has happened (avoids SSR flashes). */
  ready: boolean;
  token: string | null;
  user: User | null;
  login: (token: string, user: User) => void;
  logout: () => void;
}

/**
 * Shared auth hook — the ONLY place the web app touches the stored JWT.
 * The token lives in localStorage; every API call elsewhere takes it
 * from here and passes it to the typed client in lib/api.ts.
 */
export function useAuth(): AuthState {
  const [ready, setReady] = useState(false);
  const [token, setToken] = useState<string | null>(null);
  const [user, setUser] = useState<User | null>(null);

  useEffect(() => {
    setToken(localStorage.getItem(TOKEN_KEY));
    const raw = localStorage.getItem(USER_KEY);
    if (raw) {
      try {
        setUser(JSON.parse(raw) as User);
      } catch {
        setUser(null);
      }
    }
    setReady(true);
  }, []);

  const login = useCallback((newToken: string, newUser: User) => {
    localStorage.setItem(TOKEN_KEY, newToken);
    localStorage.setItem(USER_KEY, JSON.stringify(newUser));
    setToken(newToken);
    setUser(newUser);
  }, []);

  const logout = useCallback(() => {
    localStorage.removeItem(TOKEN_KEY);
    localStorage.removeItem(USER_KEY);
    setToken(null);
    setUser(null);
  }, []);

  return { ready, token, user, login, logout };
}
