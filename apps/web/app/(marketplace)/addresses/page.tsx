"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  createAddress,
  deleteAddress,
  getDistricts,
  listAddresses,
  setDefaultAddress,
  updateAddress,
  type Address,
  type AddressInput,
  type District,
} from "@/lib/api";
import DistrictSelect, {
  DISTRICT_ZONE_LABELS,
} from "@/components/DistrictSelect";
import { useAuth } from "@/hooks/useAuth";

/**
 * ঠিকানার বই — the buyer's address book (Wave 55): saved
 * delivery addresses with full CRUD and a default.
 *
 * The rules the page mirrors (the API enforces all of
 * them): at most 10 addresses; the first one saved becomes
 * the default automatically; deleting the default leaves
 * NO default — the buyer picks the next one themselves.
 * The zone is never a form field: it derives from the
 * district (the select lists all 64 districts with their
 * zones), which is also why the district select is the
 * only geography input here.
 *
 * Checkout consumes the same book: picking an address
 * there sends only its id, and the server takes the
 * fields from the row — so what this page shows is
 * exactly what an order will carry.
 */

const EMPTY_FORM: AddressInput = {
  label: "",
  recipient_name: "",
  phone: "",
  district: "",
  area: "",
  address_line: "",
};

const ZONE_LABELS = DISTRICT_ZONE_LABELS;

export default function AddressesPage() {
  const { ready, token } = useAuth();
  const [addresses, setAddresses] = useState<Address[] | null>(null);
  const [districts, setDistricts] = useState<District[]>([]);
  const [message, setMessage] = useState<string | null>(null);
  const [form, setForm] = useState<AddressInput>(EMPTY_FORM);
  const [formDefault, setFormDefault] = useState(false);
  const [saving, setSaving] = useState(false);
  // The address being edited inline, and its working copy.
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editForm, setEditForm] = useState<AddressInput>(EMPTY_FORM);

  const load = useCallback(async () => {
    if (!token) return;
    try {
      const book = await listAddresses(token);
      setAddresses(book.items);
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "ঠিকানা লোড করা যায়নি।"
      );
    }
  }, [token]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    let cancelled = false;
    getDistricts()
      .then((table) => {
        if (!cancelled) setDistricts(table.items);
      })
      .catch(() => {
        if (!cancelled) setDistricts([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function onCreate(e: React.FormEvent) {
    e.preventDefault();
    if (!token) return;
    setSaving(true);
    setMessage(null);
    try {
      await createAddress(token, form, formDefault);
      setForm(EMPTY_FORM);
      setFormDefault(false);
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "ঠিকানা রাখা যায়নি।");
    } finally {
      setSaving(false);
    }
  }

  function startEdit(entry: Address) {
    setEditingId(entry.id);
    setEditForm({
      label: entry.label,
      recipient_name: entry.recipient_name,
      phone: entry.phone,
      district: entry.district,
      area: entry.area,
      address_line: entry.address_line,
    });
    setMessage(null);
  }

  async function onSaveEdit(id: string) {
    if (!token) return;
    setMessage(null);
    try {
      await updateAddress(token, id, editForm);
      setEditingId(null);
      await load();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "পরিবর্তন রাখা যায়নি।");
    }
  }

  async function onDelete(id: string) {
    if (!token) return;
    setMessage(null);
    try {
      const book = await deleteAddress(token, id);
      setAddresses(book.items);
      if (editingId === id) setEditingId(null);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : "মুছে ফেলা যায়নি।");
    }
  }

  async function onSetDefault(id: string) {
    if (!token) return;
    setMessage(null);
    try {
      await setDefaultAddress(token, id);
      await load();
    } catch (err) {
      setMessage(
        err instanceof Error ? err.message : "ডিফল্ট করা যায়নি।"
      );
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
        <h1>আমার ঠিকানা — My addresses</h1>
        <p>
          ঠিকানার বই দেখতে আগে লগইন করতে হবে।{" "}
          <Link href="/login">লগইন পেজে যান — go to login</Link>
        </p>
      </main>
    );
  }

  const districtSelect = (
    value: string,
    onChange: (next: string) => void
  ) => (
    <DistrictSelect
      districts={districts}
      value={value}
      onChange={onChange}
      required
    />
  );

  const fields = (
    value: AddressInput,
    onChange: (next: AddressInput) => void
  ) => (
    <>
      <label>
        লেবেল (যেমন: বাসা, অফিস)
        <input
          value={value.label}
          onChange={(e) => onChange({ ...value, label: e.target.value })}
          maxLength={40}
          required
        />
      </label>
      <label>
        গ্রহীতার নাম
        <input
          value={value.recipient_name}
          onChange={(e) =>
            onChange({ ...value, recipient_name: e.target.value })
          }
          maxLength={80}
          required
        />
      </label>
      <label>
        মোবাইল নম্বর (01XXXXXXXXX)
        <input
          value={value.phone}
          onChange={(e) => onChange({ ...value, phone: e.target.value })}
          placeholder="01712345678"
          inputMode="tel"
          required
        />
      </label>
      <label>
        জেলা (District)
        {districtSelect(value.district, (district) =>
          onChange({ ...value, district })
        )}
      </label>
      <label>
        এলাকা / থানা
        <input
          value={value.area}
          onChange={(e) => onChange({ ...value, area: e.target.value })}
          maxLength={80}
          required
        />
      </label>
      <label>
        বিস্তারিত ঠিকানা (বাসা/হোল্ডিং, রোড)
        <input
          value={value.address_line}
          onChange={(e) =>
            onChange({ ...value, address_line: e.target.value })
          }
          maxLength={200}
          required
        />
      </label>
    </>
  );

  return (
    <main>
      <h1>আমার ঠিকানা — My addresses</h1>
      {message && <p role="alert">{message}</p>}
      <p>
        সর্বোচ্চ ১০টি ঠিকানা রাখা যায়। জেলা বেছে নিলেই ডেলিভারি জোন
        ঠিক হয়ে যায় — জোন আলাদা করে বাছতে হয় না। (Up to 10
        addresses; the delivery zone follows the district.)
      </p>

      {addresses === null ? (
        <p>লোড হচ্ছে…</p>
      ) : addresses.length === 0 ? (
        <p>
          এখনো কোনো ঠিকানা রাখা হয়নি — নিচের ফরমে প্রথমটি যোগ করুন।
          প্রথম ঠিকানাটি নিজে থেকেই ডিফল্ট হবে। (No addresses yet;
          the first one you save becomes the default.)
        </p>
      ) : (
        addresses.map((entry) => (
          <article key={entry.id} className="card">
            <h3>
              {entry.label}
              {entry.is_default && " · ডিফল্ট (Default)"}
            </h3>
            <p>
              {entry.recipient_name} · {entry.phone}
              <br />
              {entry.full_address}
              <br />
              জেলা: {entry.district_bn ?? entry.district} ({entry.district})
              · জোন: {ZONE_LABELS[entry.zone] ?? entry.zone}
              {entry.last_used_at &&
                ` · সর্বশেষ ব্যবহার: ${new Date(entry.last_used_at).toLocaleDateString("bn-BD")}`}
            </p>
            {editingId === entry.id ? (
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  void onSaveEdit(entry.id);
                }}
              >
                {fields(editForm, setEditForm)}
                <p>
                  <button type="submit">সংরক্ষণ করুন — Save</button>{" "}
                  <button type="button" onClick={() => setEditingId(null)}>
                    বাতিল — Cancel
                  </button>
                </p>
              </form>
            ) : (
              <p>
                <button type="button" onClick={() => startEdit(entry)}>
                  সম্পাদনা — Edit
                </button>{" "}
                {!entry.is_default && (
                  <>
                    <button
                      type="button"
                      onClick={() => void onSetDefault(entry.id)}
                    >
                      ডিফল্ট করুন — Make default
                    </button>{" "}
                  </>
                )}
                <button type="button" onClick={() => void onDelete(entry.id)}>
                  মুছে ফেলুন — Delete
                </button>
              </p>
            )}
          </article>
        ))
      )}

      <h2>নতুন ঠিকানা যোগ করুন — Add an address</h2>
      <form onSubmit={onCreate}>
        {fields(form, setForm)}
        <label>
          <input
            type="checkbox"
            checked={formDefault}
            onChange={(e) => setFormDefault(e.target.checked)}
          />{" "}
          এটিকে ডিফল্ট ঠিকানা করুন — Make this the default
        </label>
        <p>
          <button type="submit" disabled={saving}>
            {saving ? "রাখা হচ্ছে…" : "ঠিকানা রাখুন — Save address"}
          </button>
        </p>
      </form>
      <p>
        <Link href="/checkout">চেকআউটে ফিরুন — Back to checkout</Link>
        {" · "}
        <Link href="/orders">আমার অর্ডার — My orders</Link>
      </p>
    </main>
  );
}
