"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { requestOtp, verifyOtp } from "@/lib/api";
import { useAuth } from "@/hooks/useAuth";

/**
 * Phone + OTP login, two steps on one page:
 * 1) enter a Bangladeshi mobile number -> request an OTP;
 * 2) enter the 6-digit code -> verify -> JWT stored via useAuth.
 * In API dev mode the response carries dev_otp, shown as a hint so local
 * testing needs no real phone.
 */
export default function LoginPage() {
  const router = useRouter();
  const { login } = useAuth();
  const [phone, setPhone] = useState("");
  const [code, setCode] = useState("");
  const [step, setStep] = useState<"phone" | "code">("phone");
  const [devOtp, setDevOtp] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function onRequestOtp(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const result = await requestOtp(phone.trim());
      setPhone(result.phone);
      setDevOtp(result.dev_otp ?? null);
      setStep("code");
    } catch (err) {
      setError(
        err instanceof Error
          ? err.message
          : "কোড পাঠানো যায়নি — আবার চেষ্টা করুন।"
      );
    } finally {
      setBusy(false);
    }
  }

  async function onVerifyOtp(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const result = await verifyOtp(phone, code.trim());
      login(result.token, result.user);
      router.push("/seller");
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "কোড মেলেনি — আবার চেষ্টা করুন।"
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <main>
      <h1>লগইন — Login</h1>
      {step === "phone" ? (
        <form onSubmit={onRequestOtp}>
          <label>
            মোবাইল নম্বর (01XXXXXXXXX)
            <input
              value={phone}
              onChange={(e) => setPhone(e.target.value)}
              placeholder="01712345678"
              inputMode="tel"
              required
            />
          </label>
          <button type="submit" disabled={busy}>
            {busy ? "পাঠানো হচ্ছে…" : "OTP কোড পাঠান"}
          </button>
        </form>
      ) : (
        <form onSubmit={onVerifyOtp}>
          <p>
            {phone} নম্বরে ৬ সংখ্যার কোড পাঠানো হয়েছে। কোডটি ৫ মিনিটের মধ্যে
            ব্যবহার করুন।
          </p>
          {devOtp && (
            <p>
              <strong>ডেভ মোড:</strong> আপনার কোড <code>{devOtp}</code> (শুধু
              লোকাল টেস্টিংয়ের জন্য দেখানো হচ্ছে)
            </p>
          )}
          <label>
            OTP কোড
            <input
              value={code}
              onChange={(e) => setCode(e.target.value)}
              placeholder="123456"
              inputMode="numeric"
              required
            />
          </label>
          <button type="submit" disabled={busy}>
            {busy ? "যাচাই হচ্ছে…" : "লগইন করুন"}
          </button>
        </form>
      )}
      {error && <p role="alert">{error}</p>}
    </main>
  );
}
