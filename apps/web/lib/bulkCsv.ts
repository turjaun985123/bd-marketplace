/**
 * Client-side bulk-sheet preview (Wave 57).
 *
 * A small TypeScript mirror of the Rust domain's CSV
 * grammar and bulk header contract (crates/domain:
 * `parse_csv` + `parse_bulk_product_csv`). It exists so
 * the /seller/bulk page can tell the seller IMMEDIATELY —
 * before any upload — that the sheet's header is wrong,
 * how many data rows it carries, and which SKUs it names.
 * It is a preview, never the verdict: the API re-parses
 * and re-validates everything server-side, and the
 * import report remains the only authority.
 */

/** The bulk sheet's columns, in canonical order (the
 * domain's BULK_IMPORT_COLUMNS — keep the two in sync;
 * the server rejects an unknown column either way). */
export const BULK_IMPORT_COLUMNS = [
  "sku",
  "title",
  "title_bn",
  "description",
  "price_poisha",
  "compare_at_poisha",
  "stock",
  "weight_grams",
  "status",
  "category_slug",
  "category_label",
  "image_url",
] as const;

/** Columns a sheet MUST carry (the domain's rule). */
export const BULK_REQUIRED_COLUMNS = ["sku", "title", "price_poisha"] as const;

/** The most data rows one import may carry (domain cap). */
export const MAX_BULK_IMPORT_ROWS = 500;

/**
 * Parse CSV text into rows of fields — the same grammar
 * as the domain parser: quoted fields with `""` escapes,
 * embedded commas/newlines, CRLF/LF/CR endings, a
 * stripped BOM, and no phantom row from a trailing
 * newline. Returns null on an unterminated quote.
 */
export function parseCsvPreview(text: string): string[][] | null {
  const input = text.startsWith("\uFEFF") ? text.slice(1) : text;
  const rows: string[][] = [];
  let row: string[] = [];
  let field = "";
  let rowStarted = false;
  let state: "start" | "field" | "quotes" | "afterQuote" = "start";
  const chars = Array.from(input);
  for (let i = 0; i < chars.length; i += 1) {
    const ch = chars[i];
    if (state === "start") {
      if (ch === '"') {
        state = "quotes";
        rowStarted = true;
      } else if (ch === ",") {
        row.push(field);
        field = "";
        rowStarted = true;
      } else if (ch === "\r" || ch === "\n") {
        if (ch === "\r" && chars[i + 1] === "\n") i += 1;
        if (rowStarted || row.length > 0) {
          row.push(field);
          field = "";
          rows.push(row);
          row = [];
        }
        rowStarted = false;
      } else {
        field += ch;
        state = "field";
        rowStarted = true;
      }
    } else if (state === "field") {
      if (ch === ",") {
        row.push(field);
        field = "";
        state = "start";
      } else if (ch === "\r" || ch === "\n") {
        if (ch === "\r" && chars[i + 1] === "\n") i += 1;
        row.push(field);
        field = "";
        rows.push(row);
        row = [];
        rowStarted = false;
        state = "start";
      } else {
        field += ch;
      }
    } else if (state === "quotes") {
      if (ch === '"') state = "afterQuote";
      else field += ch;
    } else {
      // afterQuote
      if (ch === '"') {
        field += '"';
        state = "quotes";
      } else if (ch === ",") {
        row.push(field);
        field = "";
        state = "start";
      } else if (ch === "\r" || ch === "\n") {
        if (ch === "\r" && chars[i + 1] === "\n") i += 1;
        row.push(field);
        field = "";
        rows.push(row);
        row = [];
        rowStarted = false;
        state = "start";
      } else {
        field += ch;
        state = "field";
      }
    }
  }
  if (state === "quotes") return null;
  if (rowStarted || row.length > 0) {
    row.push(field);
    rows.push(row);
  }
  return rows;
}

/** What the preview can say about a sheet. */
export interface BulkSheetPreview {
  /** False when the text is not parseable CSV at all. */
  parseable: boolean;
  /** Header problems (fatal server-side), in plain words. */
  headerProblems: string[];
  /** Data rows that are not entirely blank. */
  dataRows: number;
  /** True when the sheet exceeds the 500-row cap. */
  overCap: boolean;
  /** The first few SKUs, for a sanity glance. */
  sampleSkus: string[];
  /** Rows whose cell count differs from the header's. */
  raggedRows: number;
}

/** Preview one sheet's text (header contract + counts). */
export function previewBulkSheet(text: string): BulkSheetPreview {
  const empty: BulkSheetPreview = {
    parseable: false,
    headerProblems: [],
    dataRows: 0,
    overCap: false,
    sampleSkus: [],
    raggedRows: 0,
  };
  if (!text.trim()) return { ...empty, parseable: true };
  const grid = parseCsvPreview(text);
  if (grid === null || grid.length === 0) return empty;
  const [header, ...data] = grid;
  const columns = header.map((cell) => cell.trim().toLowerCase());
  const headerProblems: string[] = [];
  for (const name of columns) {
    if (!(BULK_IMPORT_COLUMNS as readonly string[]).includes(name)) {
      headerProblems.push(`অজানা কলাম: ${name}`);
    }
  }
  for (const required of BULK_REQUIRED_COLUMNS) {
    if (!columns.includes(required)) {
      headerProblems.push(`আবশ্যক কলাম নেই: ${required}`);
    }
  }
  const seen = new Set<string>();
  for (const name of columns) {
    if (seen.has(name)) headerProblems.push(`কলাম দুইবার: ${name}`);
    seen.add(name);
  }
  const skuIndex = columns.indexOf("sku");
  const sampleSkus: string[] = [];
  let dataRows = 0;
  let raggedRows = 0;
  for (const cells of data) {
    if (cells.every((cell) => !cell.trim())) continue;
    dataRows += 1;
    if (cells.length !== columns.length) raggedRows += 1;
    if (skuIndex >= 0 && sampleSkus.length < 5) {
      const sku = (cells[skuIndex] ?? "").trim();
      if (sku) sampleSkus.push(sku);
    }
  }
  return {
    parseable: true,
    headerProblems,
    dataRows,
    overCap: dataRows > MAX_BULK_IMPORT_ROWS,
    sampleSkus,
    raggedRows,
  };
}

/** Serialise report outcomes to a downloadable CSV (the
 * seller's fix-list after a run with errors). */
export function outcomesToCsv(
  outcomes: {
    row_number: number;
    sku: string | null;
    action: string;
    errors: { field: string; message: string }[];
    notes: string[];
    image_error: string | null;
  }[]
): string {
  const escape = (value: string): string =>
    /[",\n\r]/.test(value) ? `"${value.replace(/"/g, '""')}"` : value;
  const lines = ["row,sku,action,detail"];
  for (const outcome of outcomes) {
    const details = [
      ...outcome.errors.map((error) => `${error.field}: ${error.message}`),
      ...outcome.notes,
      ...(outcome.image_error ? [`image: ${outcome.image_error}`] : []),
    ].join(" | ");
    lines.push(
      [
        String(outcome.row_number),
        escape(outcome.sku ?? ""),
        outcome.action,
        escape(details),
      ].join(",")
    );
  }
  return lines.join("\r\n") + "\r\n";
}
