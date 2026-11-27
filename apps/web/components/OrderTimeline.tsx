"use client";

import {
  shipmentStatusLabel,
  type TimelineEvent,
  type TimelineStep,
} from "@/lib/api";

/**
 * Order tracking timeline (Wave 55): the stepper + the
 * event list for one sub-order, shared by the buyer order
 * detail page and the public tracking page.
 *
 * The stepper PREFERS the API's own steps (computed by
 * the server from the domain's one derivation); the local
 * `trackingSteps` mirror below is only a fallback for a
 * view that predates the field, and is kept branch-for-
 * branch identical to the domain rule. The event list
 * renders the API's timeline verbatim: an event with
 * `at: null` (a seller's confirm, a manual delivery
 * flip — moments the platform records no timestamp for)
 * shows its step WITHOUT a time; no time is ever
 * invented here either.
 */

export type TrackingStepState = "done" | "current" | "upcoming" | "alert";

export interface TrackingStep {
  kind: string;
  state: TrackingStepState;
}

/**
 * The stepper derivation — a line-for-line mirror of the
 * domain rule (the API's tests pin the Rust side; this
 * side is display only and must not drift from it).
 */
export function trackingSteps(
  status: string,
  shipmentStatus: string | null
): TrackingStep[] {
  const step = (kind: string, state: TrackingStepState): TrackingStep => ({
    kind,
    state,
  });
  // A courier leg the courier cancelled reads as the
  // cancelled branch even before the sub-order's own
  // status catches up (unless it already delivered).
  if (
    shipmentStatus === "cancelled" &&
    status !== "delivered" &&
    status !== "returned"
  ) {
    return [
      step("placed", "done"),
      step("confirmed", "done"),
      step("cancelled", "alert"),
    ];
  }
  switch (status) {
    case "confirmed":
      return [
        step("placed", "done"),
        step("confirmed", "current"),
        step("shipped", "upcoming"),
        step("delivered", "upcoming"),
      ];
    case "shipped":
      return [
        step("placed", "done"),
        step("confirmed", "done"),
        step("shipped", "current"),
        step("delivered", "upcoming"),
      ];
    case "delivered":
      return [
        step("placed", "done"),
        step("confirmed", "done"),
        step("shipped", "done"),
        step("delivered", "done"),
      ];
    case "returned":
      return [
        step("placed", "done"),
        step("confirmed", "done"),
        step("shipped", "done"),
        step("returned", "alert"),
      ];
    case "cancelled":
      return [step("placed", "done"), step("cancelled", "alert")];
    default:
      return [
        step("placed", "current"),
        step("confirmed", "upcoming"),
        step("shipped", "upcoming"),
        step("delivered", "upcoming"),
      ];
  }
}

const STEP_LABELS: Record<string, string> = {
  placed: "অর্ডার দেওয়া হয়েছে",
  confirmed: "বিক্রেতা নিশ্চিত করেছেন",
  shipped: "পাঠানো হয়েছে",
  delivered: "ডেলিভারি হয়েছে",
  returned: "ফেরত গেছে",
  cancelled: "বাতিল হয়েছে",
};

const STEP_MARKERS: Record<TrackingStepState, string> = {
  done: "✓",
  current: "●",
  upcoming: "○",
  alert: "!",
};

/** Bangla label for one timeline event kind. */
export function timelineEventLabel(event: TimelineEvent): string {
  switch (event.kind) {
    case "placed":
    case "order_placed":
      return "অর্ডার দেওয়া হয়েছে";
    case "confirmed":
      return "বিক্রেতা নিশ্চিত করেছেন";
    case "shipment_booked":
      return "কুরিয়ার বুক হয়েছে";
    case "shipment_status":
      return `কুরিয়ার: ${event.status ? shipmentStatusLabel(event.status) : ""}`;
    case "delivered":
      return "ডেলিভারি হয়েছে";
    case "returned":
      return "পণ্য ফেরত গেছে";
    case "cancelled":
      return "অর্ডার বাতিল হয়েছে";
    case "return_requested":
      return "রিটার্নের আবেদন করা হয়েছে";
    case "return_decided":
      return "রিটার্নের সিদ্ধান্ত হয়েছে";
    case "payment_initiated":
      return "পেমেন্ট শুরু হয়েছে";
    case "payment_paid":
      return "পেমেন্ট সম্পন্ন হয়েছে";
    case "advance_initiated":
      return "অগ্রিম পেমেন্ট শুরু হয়েছে";
    case "advance_paid":
      return "অগ্রিম পরিশোধ হয়েছে";
    default:
      return event.kind;
  }
}

/**
 * The stepper + event list for one sub-order. The parent
 * supplies the API timeline for the events and, normally,
 * the API-computed steps; the status pair is the fallback
 * derivation input.
 */
export default function OrderTimeline({
  status,
  shipmentStatus,
  events,
  steps: apiSteps,
}: {
  status: string;
  shipmentStatus: string | null;
  events: TimelineEvent[];
  steps?: TimelineStep[];
}) {
  const steps: TrackingStep[] =
    apiSteps && apiSteps.length > 0
      ? apiSteps.map((step) => ({
          kind: step.kind,
          state: step.state as TrackingStepState,
        }))
      : trackingSteps(status, shipmentStatus);
  return (
    <div>
      <ol className="timeline-steps">
        {steps.map((step) => (
          <li key={step.kind} data-state={step.state}>
            <span aria-hidden="true">{STEP_MARKERS[step.state]}</span>{" "}
            {STEP_LABELS[step.kind] ?? step.kind}
          </li>
        ))}
      </ol>
      <ul className="timeline-events">
        {events.map((event, idx) => (
          <li key={`${event.kind}-${idx}`}>
            {timelineEventLabel(event)}
            {event.note && <> — {event.note}</>}
            {event.at && (
              <>
                {" "}
                · {new Date(event.at).toLocaleString("bn-BD")}
              </>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}
