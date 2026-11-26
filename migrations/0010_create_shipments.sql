-- Wave 6: shipments + shipment_events — one courier booking per
-- sub_order, tracked in the unified shipment status set (the pure
-- domain functions is_valid_shipment_status / can_transition_shipment
-- / map_steadfast_status are the single source of truth for the set,
-- the legal moves and the Steadfast raw-status mapping).
-- One shipment per sub-order is a hard rule for now (UNIQUE on
-- sub_order_id): a seller books once; a returned parcel that needs
-- re-shipping is a later wave's problem, not a second row here.
-- shipment_events is the audit trail: every status the shipment
-- takes is recorded with the courier's raw status (when the courier
-- provided one) and the source of the change (create / refresh /
-- manual). All money is integer poisha (bigint).

CREATE TABLE shipments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    sub_order_id uuid NOT NULL UNIQUE REFERENCES sub_orders (id) ON DELETE CASCADE,
    courier text NOT NULL,
    consignment_id text NOT NULL,
    tracking_code text,
    status text NOT NULL DEFAULT 'created'
        CHECK (status IN ('created', 'picked_up', 'in_transit', 'out_for_delivery',
                          'delivered', 'partial_delivered', 'return_in_transit',
                          'returned', 'cancelled', 'unknown')),
    cod_amount_poisha bigint NOT NULL CHECK (cod_amount_poisha >= 0),
    note text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX shipments_consignment_id_idx ON shipments (consignment_id);

CREATE TABLE shipment_events (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    shipment_id uuid NOT NULL REFERENCES shipments (id) ON DELETE CASCADE,
    status text NOT NULL,
    raw_status text,
    source text NOT NULL CHECK (source IN ('create', 'refresh', 'manual')),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX shipment_events_shipment_id_idx ON shipment_events (shipment_id, created_at);
