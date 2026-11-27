"use client";

/**
 * The offline fallback page (Wave 38). The service
 * worker precaches /offline at install and serves it
 * when a navigation fails with nothing cached. It must
 * never depend on API data — everything here is static,
 * and the only action is a plain reload.
 */
export default function OfflinePage() {
  return (
    <main>
      <h1>আপনি এখন অফলাইন</h1>
      <p>
        আপনি এখন অফলাইন — ইন্টারনেট ফিরলেই আবার কেনাকাটা করা যাবে। You are
        offline right now — shopping resumes as soon as the internet is back.
      </p>
      <p>
        আগে দেখা পণ্যের পেজগুলো হয়তো এখনো খুলবে — সেগুলো আপনার ফোনে রাখা
        কপি। কার্ট আর চেকআউটের জন্য ইন্টারনেট লাগবেই। Pages you already
        visited may still open from the copy on your phone; cart and checkout
        always need a connection.
      </p>
      <p>
        <button type="button" onClick={() => window.location.reload()}>
          আবার চেষ্টা করুন — Try again
        </button>
      </p>
    </main>
  );
}
