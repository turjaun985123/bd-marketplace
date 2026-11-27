/**
 * The built-in category icon set (Wave 56): the API
 * stores an `icon_key` per category — a KEY, never an
 * image — and this map is the web's rendering of the
 * keys. The key set mirrors the domain crate's
 * `CATEGORY_ICON_KEYS`; an unknown key falls back to the
 * tag glyph, so a category added server-side with a key
 * this build does not know still renders something sane.
 *
 * Glyphs are plain unicode marks inside a styled span —
 * the same dependency-free approach as the rest of the
 * storefront's iconography.
 */
const CATEGORY_ICON_GLYPHS: Record<string, string> = {
  tag: "🏷️",
  shirt: "👕",
  phone: "📱",
  cart: "🛒",
  basket: "🧺",
  home: "🏠",
  sparkles: "✨",
  baby: "👶",
  trophy: "🏆",
  car: "🚗",
  leaf: "🌿",
  book: "📚",
  grid: "🔲",
  watch: "⌚",
  shoe: "👟",
  laptop: "💻",
  heart: "❤️",
  gift: "🎁",
};

/** The glyph for an icon key (tag fallback for unknown keys). */
export function categoryIconGlyph(iconKey: string): string {
  return CATEGORY_ICON_GLYPHS[iconKey] ?? CATEGORY_ICON_GLYPHS.tag;
}

/** Every key the picker/admin icon select offers, in set order. */
export const CATEGORY_ICON_KEYS = Object.keys(CATEGORY_ICON_GLYPHS);

export default function CategoryIcon({
  iconKey,
  className,
}: {
  iconKey: string;
  className?: string;
}) {
  return (
    <span className={className} role="img" aria-label={iconKey}>
      {categoryIconGlyph(iconKey)}
    </span>
  );
}
