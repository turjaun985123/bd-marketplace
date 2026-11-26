export default function StorefrontPage({
  params,
}: {
  params: { store: string };
}) {
  return (
    <main>
      <h1>স্টোর: {params.store}</h1>
      <p>
        এটি <strong>{params.store}</strong>-এর নিজস্ব স্টোরফ্রন্ট — একই পণ্য
        কেন্দ্রীয় মার্কেটপ্লেসেও পাওয়া যায়।
      </p>
      <p>
        Per-seller storefront placeholder (Wave 1). In production this route
        group is also served via subdomain rewrite, e.g.{" "}
        <code>{params.store}.shop.example.bd</code>.
      </p>
    </main>
  );
}
