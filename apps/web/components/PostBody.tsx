/**
 * A content body, rendered (Wave 59): store pages and
 * blog posts store PLAIN TEXT — this component splits
 * it into paragraphs on blank lines and renders each
 * as a <p>, with single line breaks inside a paragraph
 * preserved by the `post-body` CSS (`pre-line`). Seller
 * text is never interpreted as markup anywhere in the
 * content system: there is no dangerouslySetInnerHTML
 * here by design, so a seller typing "<b>bold</b>"
 * shows their readers exactly those characters.
 */
export default function PostBody({ body }: { body: string }) {
  const paragraphs = body
    .split(/\n\s*\n/)
    .map((paragraph) => paragraph.trim())
    .filter((paragraph) => paragraph.length > 0);
  return (
    <div className="post-body">
      {paragraphs.map((paragraph, index) => (
        <p key={index}>{paragraph}</p>
      ))}
    </div>
  );
}
