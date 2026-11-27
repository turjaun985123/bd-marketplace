/**
 * ProductImage (Wave 51): the one way product imagery renders
 * anywhere in the app — a plain <img> over the API's derived
 * sizes with lazy loading, or, when the product has no image,
 * an initial-letter placeholder tile in the brand palette.
 * Callers pass the URLs the API already chose (the product's
 * `image_url`, or a gallery view's sizes for a srcset).
 */
export default function ProductImage({
  src,
  srcSet,
  sizes,
  alt,
  placeholderText,
  className,
}: {
  /** The default (gallery-size) URL; null renders the placeholder. */
  src: string | null;
  /** Optional responsive candidates, e.g. "…-300.webp 300w, …-600.webp 600w". */
  srcSet?: string;
  sizes?: string;
  alt: string;
  /** Text whose first letter fills the placeholder tile. */
  placeholderText?: string;
  className?: string;
}) {
  if (!src) {
    const letter = (placeholderText ?? alt).trim().charAt(0) || "প";
    return (
      <div
        className={`product-image-placeholder ${className ?? ""}`}
        role="img"
        aria-label={alt}
      >
        <span aria-hidden="true">{letter}</span>
      </div>
    );
  }
  return (
    // The API serves its own derived sizes with immutable
    // caching; a plain <img> keeps every deployment (including
    // the offline PWA shell) free of an image-optimizer host.
    // eslint-disable-next-line @next/next/no-img-element
    <img
      className={`product-image ${className ?? ""}`}
      src={src}
      srcSet={srcSet}
      sizes={sizes}
      alt={alt}
      loading="lazy"
      decoding="async"
    />
  );
}
