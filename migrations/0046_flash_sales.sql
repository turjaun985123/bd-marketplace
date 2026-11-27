-- Wave 52: flash sales — platform-wide, time-boxed price
-- events. A flash sale names a window and a set of products,
-- each with its own sale price (poisha, strictly between 0 and
-- the product's base price — validated at write time against
-- the product row by the API) and an optional `stock_cap`:
-- the maximum number of units that may be sold AT the sale
-- price (null = uncapped).
--
-- At most ONE flash sale is live at any instant. That rule is
-- enforced by the create/update service function inside a
-- transaction (overlapping ACTIVE windows are refused) — it
-- is a cross-row rule over a time range, which a CHECK or
-- UNIQUE constraint cannot express.
--
-- Cap accounting is a definition, not a stored counter: units
-- sold at the sale price are the SUM of quantities over order
-- items in non-cancelled sub-orders whose parent order was
-- placed inside the sale window and whose snapshot unit price
-- equals the price the flash resolution produced for that
-- line (the sale price, plus the variant's price delta for
-- variant lines). The one aggregate lives in the API's merch
-- module (`flash_units_sold_on`); when the count reaches the
-- cap the price reverts to base everywhere. See the README's
-- Wave 52 section.

CREATE TABLE flash_sales (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    title text NOT NULL,
    starts_at timestamptz NOT NULL,
    ends_at timestamptz NOT NULL,
    is_active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT flash_sales_window_order CHECK (ends_at > starts_at)
);

CREATE TABLE flash_sale_items (
    flash_sale_id uuid NOT NULL REFERENCES flash_sales (id) ON DELETE CASCADE,
    product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    sale_price_poisha bigint NOT NULL CHECK (sale_price_poisha > 0),
    stock_cap integer NULL CHECK (stock_cap IS NULL OR stock_cap > 0),
    PRIMARY KEY (flash_sale_id, product_id)
);

-- The live-sale lookup and the per-product item lookup.
CREATE INDEX flash_sales_active_window
    ON flash_sales (is_active, starts_at, ends_at);
CREATE INDEX flash_sale_items_product
    ON flash_sale_items (product_id);
