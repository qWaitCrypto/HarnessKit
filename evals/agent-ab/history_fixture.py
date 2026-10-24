"""Product changes retained in the generated service's Git history."""

from __future__ import annotations


def history_updates(scattered: bool) -> list[tuple[str, str, str]]:
    # A loose-note counterpart keeps S6's history in its original flat layout.
    entries = (
        ("Describe merchant onboarding inputs", "product-specs/checkout", "merchant-onboarding", "Onboarding records the merchant region and settlement currency before accepting a catalog import."),
        ("Record invoice rounding example", "decisions/005-money", "tax-rounding", "An invoice for 1,800 cents at 800 basis points carries 144 cents of tax; rounding occurs once on the invoice subtotal."),
        ("Keep capture references on review cases", "notes/provider-outage", "provider-review", "Case PAY-217 retained the provider reference and the timeout timestamp beside the pending ledger row."),
        ("Add support escalation identifiers", "operations/order-oncall", "support-scope", "Escalations include the merchant account, support scope expiry, and the operator's incident case number."),
        ("Explain regional authorization routing", "design-docs/payment-routing", "region-routing", "Cedar authorizations use the UK adapter; a region change applies to new orders after the configuration snapshot is activated."),
        ("Separate pool wait from edge saturation", "notes/october-capacity", "database-pool", "The load trial reached a 12 ms pool wait while edge buckets rejected a burst; adding workers did not change that edge response."),
        ("Document customer export pagination", "product-specs/tenant-billing", "export-manifest", "Export pages carry a manifest id and a last-order cursor so an interrupted download resumes against the same sealed snapshot."),
        ("Retain the partner duplicate sample", "notes/webhook-duplicate", "webhook-ack", "Partner delivery wh-812 arrived twice 14 seconds apart; only one invoice notification was emitted."),
        ("Describe replay case annotations", "operations/event-replay", "replay-window", "The replay case records the event id, consumer name, original offset, and the operator who released it."),
        ("Add trace retention guidance", "design-docs/observability", "trace-shape", "Gateway and worker spans retain a common correlation id for seven days, while payment audit records follow the finance retention period."),
        ("Record optional envelope field rehearsal", "notes/schema-rehearsal", "schema-rehearsal", "The rehearsal added an optional merchant_region field; the previous reader accepted envelopes both with and without it."),
        ("Document pool acquisition timeout", "operations/database-connections", "database-pool", "Pool acquisition times out after two seconds and reports a dependency-unavailable response with the request correlation id."),
        ("Describe billing manifest checksums", "product-specs/tenant-billing", "customer-manifest", "A manifest checksum covers the sorted order ids and minor-unit amounts, excluding the download URL expiry."),
        ("Record stale cart reservation cleanup", "design-docs/order-lifecycle", "cart-expiry", "Reservation cleanup releases inventory after cart expiry and leaves the order transition record available to support."),
        ("Add currency checks to refund handoff", "operations/payment-operations", "refund-audit", "Refund review compares the requested currency with the original capture currency before the worker is released."),
        ("Explain worker lease renewal", "operations/queue-operations", "queue-leases", "A worker renews its 45-second lease before a slow provider lookup; ownership remains with the same worker id."),
        ("Record capture timeout lookup outcome", "notes/provider-outage", "provider-timeout", "The provider lookup returned a successful capture for the timed-out request, so the pending row was completed without sending another charge."),
        ("Separate cache expiry from ledger retention", "design-docs/cache-layout", "cache-keys", "Cache entries expire after fifteen minutes; expiry does not delete the order or its settlement evidence."),
        ("Describe fraud review threshold", "design-docs/payment-routing", "fraud-check", "Orders above 500,000 minor units are held for fraud review before authorization reaches the payment adapter."),
        ("Add shipping timeline evidence", "design-docs/order-lifecycle", "shipping-adapter", "The shipping timeline stores a carrier reference separately from authorization and capture references."),
        ("Record session incident correlation sample", "notes/session-incident", "session-expiry", "The credential incident lasted 38 seconds; the retained sample connects six expired sessions to three merchant accounts."),
        ("Describe release smoke evidence", "operations/release-process", "release-owner", "Release evidence includes one accepted checkout, one captured payment, and the tenant-scoped settlement export checksum."),
        ("Define operator console redaction", "operations/tenant-boundaries", "admin-console", "The support console masks customer email addresses in exported screenshots while preserving incident and provider references."),
        ("Record queue age distribution", "notes/queue-load", "queue-sample", "At the October trial, queue depth was 218 and the oldest unleased item was 11 seconds old; lease age identified the slow adapter."),
        ("Describe reader compatibility window", "decisions/006-schema-evolution", "migration-window", "The previous event reader remains enabled for one release cycle after the writer starts emitting the optional field."),
        ("Record tenant scope audit sample", "notes/tenant-support", "tenant-storage", "The Cedar audit sampled 24 support sessions and found that each retained its explicit operator scope."),
        ("Explain batch mismatch evidence", "design-docs/settlement-reconciliation", "reconciliation", "Review items attach the provider business day, both row totals, and the manifest id used by the comparison."),
        ("Retain catalog prices on line items", "product-specs/checkout", "line-items", "An accepted line item preserves its catalog price even when a later catalog import changes the merchant's current offer."),
        ("Describe second reviewer worksheet", "operations/release-process", "second-reviewer", "The reviewer signs the tenant sample and notes any migration warnings in the change record before the traffic switch."),
        ("Add request error identifiers", "references/api-errors", "gateway-errors", "Client errors carry stable reason codes; on-call staff correlate them with server spans using the returned request id."),
        ("Record backup restore scope checks", "operations/tenant-boundaries", "backup-restore", "The restore exercise sampled Northwind and Cedar independently and compared their order counts with the signed backup manifests."),
        ("Explain health probe dependency states", "operations/order-oncall", "health-check", "Readiness reflects whether this process can accept requests; dependency health is reported separately for the payment provider and database."),
        ("Document worker drain window", "operations/queue-operations", "worker-drain", "Deployment waits for leased work to finish while stopping new worker acquisitions; queued work remains durable."),
        ("Record ledger seal before export", "notes/settlement-handoff", "ledger-snapshot", "The ledger sample was sealed under manifest nw-oct-18 and attached to case SET-63 before comparison with the provider rows."),
        ("Describe configuration snapshot activation", "design-docs/observability", "config-refresh", "Configuration refresh validates the incoming map and swaps an immutable snapshot after all region mappings are present."),
        ("Record quota dashboard units", "notes/october-capacity", "tenant-quota", "The tenant quota panel shows orders per business day; the gateway panel shows requests per minute and the pool panel shows active connections."),
        ("Describe independent transport counters", "design-docs/retry-budgets", "retry-boundaries", "The HTTP client records retry attempts on 408, 429, and 503; the worker delivery counter advances only after a job lease."),
        ("Record dead letter review ownership", "operations/queue-operations", "dead-letter-review", "The on-call payment owner groups dead-letter captures by provider region before opening a replay case."),
        ("Describe retention for signed exports", "product-specs/tenant-billing", "retention", "A signed export manifest remains available after the download URL expires, so finance can request another link to the same rows."),
        ("Record October staging tenant selection", "notes/settlement-handoff", "launch-window", "Northwind staging is reserved for the October rehearsal; production export signing remains with the finance operations shift."),
        ("Document invoice support evidence", "operations/order-oncall", "currency", "Invoice support attaches the original minor-unit amount, currency code, and checkout timestamp to the customer case."),
        ("Retain provider review discussion", "notes/provider-outage", "incident-links", "The review minutes link case PAY-217 to the regional outage timeline and the retained gateway traces."),
        ("Describe capture and authorization references", "decisions/001-payment-capture", "authorization", "Authorization and capture identifiers occupy separate audit fields even when a provider returns the same string for both operations."),
        ("Record settlement shift handoff", "notes/settlement-handoff", "capture-worker", "The outgoing shift left the provider CSV attached to SET-63 and handed its manifest checksum to the next operator."),
    )
    return [
        (message, f"notes/{note}.md" if scattered else f"docs/{doc}.md", paragraph)
        for message, doc, note, paragraph in entries
    ]
