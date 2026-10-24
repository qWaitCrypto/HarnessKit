"""Deterministic, product-shaped repositories for the E1 agent evaluation.

The fixture is Harbourline, a multi-tenant order and payment service.  Its
documents are useful operational material and its Rust modules form a small
callable service graph.  Scenario facts live in the fixture, not in prompts.
"""

from __future__ import annotations

import hashlib
import os
import re
import shutil
import subprocess
import tarfile
import textwrap
from dataclasses import dataclass
from datetime import date, timedelta
from pathlib import Path


ROOT = Path(__file__).resolve().parent
SCENARIO_ROOT = ROOT / "scenarios"


@dataclass(frozen=True)
class Scenario:
    scenario_id: str
    prompt: str
    key_files: tuple[str, ...]
    constraint_files: tuple[str, ...]
    arms: tuple[str, ...] = ("A", "B")
    title: str = ""
    focus: str = ""


SCENARIOS: dict[str, Scenario] = {
    "S1": Scenario(
        "S1",
        "订单服务里的会话续期偶尔失败，给它加个自动重试",
        ("src/auth/refresh.rs", "docs/decisions/002-session-renewal.md"),
        ("docs/decisions/002-session-renewal.md",),
        title="会话续期的边界",
        focus="刷新路径的单飞约束，以及调用方与刷新器的责任边界",
    ),
    "S2": Scenario(
        "S2",
        "billing 切到 v8 之后订单列表读不到新写入的缓存，修一下",
        ("src/cache/keys.rs", "src/cache/settlement.rs", "docs/notes/cache-observation.md"),
        ("docs/notes/cache-observation.md",),
        title="结算摘要缓存",
        focus="两个真实消费者必须一起迁移缓存命名空间",
    ),
    "S3": Scenario(
        "S3",
        "接着上次没做完的工作继续做",
        ("docs/exec-plans/active/settlement-cutover.md", "src/settlement/window.rs"),
        ("docs/exec-plans/active/settlement-cutover.md", "docs/worklog/active/2026-10-18-settlement.md"),
        title="接手结算改造",
        focus="两个计划都留在 active 目录时，按生命周期和工作日志选择下一步",
    ),
    "S4": Scenario(
        "S4",
        "网关限流阈值是多少？写给运维一段说明",
        ("src/gateway/rate_limit.rs", "docs/operations/gateway-throttles.md"),
        ("docs/operations/gateway-throttles.md",),
        title="网关运行说明",
        focus="代码曾在文档之后调整，运维说明需要以当前实现为准",
    ),
    "S5": Scenario(
        "S5",
        "把队列满时的返回改成 503，顺便把文档也更新一下",
        ("src/queue/admission.rs", "docs/design-docs/queue.md", "docs/index.md"),
        ("docs/design-docs/queue.md", "docs/design-docs/queue-v1.md"),
        title="队列满载契约",
        focus="现行队列契约与保留的 v1 文档同时出现时的维护范围",
    ),
    "S6": Scenario(
        "S6",
        "帮这个项目把文档体系建起来",
        ("README.md", "notes/", "AGENTS.md"),
        ("README.md", "ARCHITECTURE.md"),
        title="整理 Harbourline 文档",
        focus="把散落的产品事实接入入口、保留上下文并消除重复段落",
    ),
    "S7": Scenario(
        "S7",
        "把 harnesskit 升级到最新并处理迁移",
        (".harnesskit/schema.yaml", "agent-surfaces/skills/harnesskit/SKILL.md"),
        (".harnesskit/schema.yaml", "agent-surfaces/skills/harnesskit/SKILL.md"),
        arms=("B",),
        title="离线迁移 HarnessKit",
        focus="旧 schema、旧 skill、references 和本地 release 的同步",
    ),
}


# Each fact requires all concepts, with alternatives for natural English and
# Chinese wording. A bag of unrelated matching words across files is not a fact.
# Field notes whose only paragraph restates an invariant that has a canonical
# home elsewhere; a clean S6 keeps one copy of it, the rest of the notes stay.
S6_FACT_NOTES = ("order-creation", "event-delivery", "tenant-storage", "job-failures", "production-review")

S6_FACT_GROUPS: dict[str, tuple[tuple[str, ...], ...]] = {
    "order idempotency": (("order creation", "creating an order", "checkout", "订单创建", "创建订单"),
                          ("idempotency", "idempotent", "幂等")),
    "event delivery": (("event bus", "events", "event delivery", "事件总线", "事件投递"),
                       ("at-least-once", "at least once", "至少一次")),
    "tenant isolation": (("tenant_id", "tenant id", "租户标识"),
                         ("isolated", "isolation", "separated", "隔离")),
    "failed jobs": (("background jobs", "background tasks", "后台任务"),
                    ("fail", "失败"), ("dead-letter", "dead letter", "死信")),
    "release review": (("production release", "production deployment", "生产发布", "生产部署"),
                       ("two-person", "two person", "second reviewer", "双人", "两人"),
                       ("review", "approval", "复核", "审批", "审核")),
}


def scenario_ids(value: str) -> list[str]:
    if value.lower() == "all":
        return list(SCENARIOS)
    ids = [part.strip().upper() for part in value.split(",") if part.strip()]
    unknown = sorted(set(ids) - set(SCENARIOS))
    if unknown:
        raise ValueError(f"unknown scenario(s): {', '.join(unknown)}")
    return ids


def _write(root: Path, relative: str, content: str) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(textwrap.dedent(content).lstrip(), encoding="utf-8")


def _rust_files() -> dict[str, str]:
    from service_fixture import rust_files

    return rust_files()


def _rust_tools() -> dict[str, str]:
    return {
        "tools/reconciliation_report.py": """
            from dataclasses import dataclass

            CACHE_NAMESPACE_VERSION = 7

            @dataclass(frozen=True)
            class SettlementRow:
                tenant_id: str
                amount_cents: int

            def cache_key(tenant_id: str, day: str) -> str:
                return f"settlement:v{CACHE_NAMESPACE_VERSION}:{tenant_id}:{day}"

            def net_amount(rows: list[SettlementRow]) -> int:
                return sum(row.amount_cents for row in rows)
        """,
        "tools/refresh_config.py": """
            def refresh_from_env(current: dict[str, str], incoming: dict[str, str]) -> dict[str, str]:
                merged = dict(current)
                merged.update(incoming)
                return merged
        """,
        "tools/queue_inspector.py": """
            def queue_status(waiting: int) -> int:
                return 503 if waiting > 1000 else 200
        """,
    }


def _fixture_support() -> dict[str, str]:
    """Real configuration, migration, API and integration-test artifacts."""

    files: dict[str, str] = {
        "tests/order_flow.rs": """
            use harbourline_order_service::process_order;

            #[test]
            fn order_flow_composes_domain_steps() {
                assert!(process_order(10) > 10);
            }
        """,
        "tests/cache_contract.rs": """
            use harbourline_order_service::cache::{keys, settlement};

            #[test]
            fn cache_keys_keep_tenant_and_business_suffixes() {
                assert!(keys::namespace_key("cedar", "order-3").ends_with(":cedar:order-3"));
                assert!(settlement::settlement_key("cedar", "2026-10-18").ends_with(":cedar:2026-10-18"));
            }
        """,
        "tests/queue_contract.rs": """
            use harbourline_order_service::queue::admission::full_response;

            #[test]
            fn admission_uses_a_documented_http_status() {
                assert!(matches!(full_response(), 429 | 503));
            }
        """,
        "migrations/2025-01-orders.sql": """
            create table orders (tenant_id text not null, order_id text not null, state text not null);
            create unique index orders_tenant_key on orders (tenant_id, order_id);
        """,
        "migrations/2025-02-idempotency.sql": """
            create table idempotency_keys (tenant_id text not null, key text not null, outcome text not null);
            create unique index idempotency_tenant_key on idempotency_keys (tenant_id, key);
        """,
        "migrations/2025-03-payments.sql": """
            create table payments (tenant_id text not null, order_id text not null, provider_ref text);
            create index payments_provider_ref on payments (provider_ref);
        """,
        "migrations/2025-04-events.sql": """
            create table event_receipts (tenant_id text not null, event_id text not null, received_at integer not null);
            create unique index event_receipts_key on event_receipts (tenant_id, event_id);
        """,
        "migrations/2025-05-settlement.sql": """
            create table settlement_rows (tenant_id text not null, business_day text not null, amount_cents integer not null);
            create index settlement_day on settlement_rows (tenant_id, business_day);
        """,
        "migrations/2025-06-support-scope.sql": """
            create table support_scopes (operator_id text not null, tenant_id text not null, expires_at integer not null);
        """,
        "migrations/2025-07-manifests.sql": """
            create table export_manifests (tenant_id text not null, manifest_id text not null, checksum text not null);
        """,
        "migrations/2025-08-review-items.sql": """
            create table review_items (tenant_id text not null, provider_ref text, reason text not null);
        """,
        "migrations/2025-09-audit.sql": """
            create table audit_entries (tenant_id text not null, correlation_id text not null, action text not null);
            create index audit_entries_tenant on audit_entries (tenant_id, correlation_id);
        """,
        "migrations/2025-10-queue.sql": """
            create table queue_leases (job_id text not null, worker_id text not null, expires_at integer not null);
        """,
        "config/service.toml": """
            service_name = "harbourline-orders"
            envelope_version = 4
        """,
        "config/observability.toml": """
            correlation_header = "x-correlation-id"
            redact_customer_identifiers = true
        """,
        "config/queue.toml": """
            worker_lease_seconds = 45
            max_delivery_attempts = 5
        """,
        "api/schemas/order.json": """
            {"type":"object","required":["tenant_id","idempotency_key","lines"]}
        """,
        "api/schemas/payment.json": """
            {"type":"object","required":["order_id","provider_ref","amount_cents"]}
        """,
        "api/schemas/event.json": """
            {"type":"object","required":["event_id","tenant_id","version"]}
        """,
        "tests/settlement_flow.rs": """
            use harbourline_order_service::settlement::window::settlement_window_seconds;

            #[test]
            fn settlement_window_is_positive() {
                assert!(settlement_window_seconds() > 0);
            }
        """,
        "tests/merchant_flow.rs": """
            use harbourline_order_service::{auth, cache, clients, config, domain, observability, payments, queue, storage};

            #[test]
            fn fulfillment_keeps_customer_and_provider_scope() {
                let tenant = auth::claims::tenant_claim("cedar").unwrap();
                assert_eq!(domain::customer::scoped_customer(&tenant, "c-18").unwrap(), "cedar:c-18");
                let mut order = domain::order::create(tenant, "order-18", &[("canvas-bag", 2)]).unwrap();
                let mut provider = payments::provider::Provider::default();
                let reference = queue::worker::deliver(&mut order, &mut provider, 0).unwrap();
                assert_eq!(clients::shipping::shipment_reference(&order, "GB").unwrap(), "shipment:cedar:order-18");
                assert_eq!(payments::refund::refund_reference(&order, &reference).unwrap(), "refund:capture:order-18");
            }

            #[test]
            fn cache_eviction_preserves_other_merchants() {
                let mut entries = cache::entries::Entries::default();
                let cedar = cache::keys::namespace_key("cedar", "order-1");
                let northwind = cache::keys::namespace_key("northwind", "order-1");
                entries.put(cedar.clone(), "stale".into());
                entries.put(northwind.clone(), "current".into());
                cache::invalidation::invalidate_order(&mut entries, "cedar", "order-1");
                assert!(entries.get(&cedar).is_none());
                assert_eq!(entries.get(&northwind).unwrap(), "current");
            }

            #[test]
            fn configuration_refresh_leaves_readers_on_their_snapshot() {
                let current = std::collections::BTreeMap::from([("region".into(), "UK".into())]);
                let next = config::snapshot::refresh(&current, std::collections::BTreeMap::from([("region".into(), "EU".into())]));
                assert_eq!(current["region"], "UK");
                assert_eq!(next["region"], "EU");
                assert!(storage::pool::can_acquire(39));
                assert!(!storage::pool::can_acquire(40));
            }

            #[test]
            fn admission_metrics_and_request_session() {
                let mut metrics = observability::metrics::Metrics::default();
                metrics.observe(202);
                metrics.observe(429);
                assert_eq!((metrics.accepted_orders, metrics.rejected_orders), (1, 1));
                let mut state = auth::refresh::RefreshState::default();
                assert!(harbourline_order_service::gateway::session_access::request_session(&mut state, "merchant").is_ok());
            }
        """,
    }
    regions = {
        "northwind": ("USD", "primary-us"),
        "larkspur": ("EUR", "primary-eu"),
        "cedar": ("GBP", "primary-uk"),
        "juniper": ("CAD", "primary-ca"),
        "mariner": ("AUD", "primary-au"),
        "orchard": ("JPY", "primary-jp"),
    }
    for tenant, (currency, provider) in regions.items():
        files[f"config/tenants/{tenant}.toml"] = f"tenant = \"{tenant}\"\ncurrency = \"{currency}\"\nprovider = \"{provider}\"\n"
    event_examples = {
        "order-accepted": "accepted",
        "payment-authorized": "authorized",
        "payment-captured": "captured",
        "settlement-reviewed": "reviewed",
        "export-signed": "signed",
        "order-cancelled": "cancelled",
    }
    for name, state in event_examples.items():
        files[f"api/examples/{name}.json"] = f'{{"event_id":"evt-{name}","tenant_id":"northwind","state":"{state}","version":4}}\n'
    files["scripts/check-ledger.sh"] = "#!/usr/bin/env bash\nset -euo pipefail\ncargo test --test settlement_flow\n"
    files["scripts/validate-schema.sh"] = """
        #!/usr/bin/env bash
        set -euo pipefail
        python3 - <<'PY'
        import json
        from pathlib import Path
        for path in Path('api/schemas').glob('*.json'):
            schema = json.loads(path.read_text())
            assert schema['type'] == 'object' and schema['required'], path
        PY
    """
    return files


def _doc_indexes() -> dict[str, str]:
    return {
        "docs/index.md": """
            # Harbourline knowledge map

            The [project map](project.md) describes the current delivery slice.
            Use the [design index](design-docs/index.md) for service contracts,
            [decisions](decisions/index.md) for accepted policy, [operations]
            (operations/index.md) for live procedures, and [notes](notes/index.md)
            for incident context. Current work lives in [active plans]
            (exec-plans/active/index.md); [completed plans]
            (exec-plans/completed/index.md) remain available for history.

            [Product specifications](product-specs/index.md), [references]
            (references/index.md), and the [active worklog]
            (worklog/active/index.md) complete the route for a new maintainer.

            - [Queue behavior](design-docs/queue.md)
            - [Queue behavior](design-docs/queue-v1.md)
        """,
        "docs/project.md": """
            # Project map

            Harbourline is preparing the October settlement cycle for the
            Northwind, Larkspur, and Cedar tenants. The current slice connects
            order admission, provider capture, and a daily settlement export.

            The [settlement cutover plan](exec-plans/active/settlement-cutover.md)
            is the active delivery thread. The [October worklog]
            (worklog/active/2026-10-18-settlement.md) records where the previous
            shift stopped; runbooks are under [operations](operations/index.md).
        """,
        "docs/design-docs/index.md": """
            # Design documents

            - [Order lifecycle](order-lifecycle.md)
            - [Payment routing](payment-routing.md)
            - [Tenant boundaries](tenant-isolation.md)
            - [Webhook delivery](webhook-delivery.md)
            - [Queue behavior](queue.md)
            - [Queue behavior](queue-v1.md)
            - [Retry budgets](retry-budgets.md)
            - [Cache layout](cache-layout.md)
            - [Settlement reconciliation](settlement-reconciliation.md)
            - [Observability](observability.md)
        """,
        "docs/decisions/index.md": """
            # Decisions

            - [Idempotency](000-idempotency.md)
            - [Payment capture](001-payment-capture.md)
            - [Session renewal](002-session-renewal.md)
            - [Tenant data](003-tenant-data.md)
            - [Event delivery](004-event-delivery.md)
            - [Money representation](005-money.md)
            - [Schema evolution](006-schema-evolution.md)
            - [Reconciliation](007-reconciliation.md)
        """,
        "docs/operations/index.md": """
            # Operations

            - [Gateway throttles](gateway-throttles.md)
            - [Order on-call](order-oncall.md)
            - [Payment operations](payment-operations.md)
            - [Queue operations](queue-operations.md)
            - [Tenant boundaries](tenant-boundaries.md)
            - [Database connections](database-connections.md)
            - [Event replay](event-replay.md)
            - [Release process](release-process.md)
            - [Cache operations](cache-operations.md)
            - [Session operations](session-operations.md)
            - [Settlement operations](settlement-operations.md)
        """,
        "docs/notes/index.md": """
            # Notes

            - [Cache observation](cache-observation.md)
            - [Provider outage review](provider-outage.md)
            - [Queue load sample](queue-load.md)
            - [Tenant support case](tenant-support.md)
            - [Settlement handoff](settlement-handoff.md)
            - [Webhook duplicate](webhook-duplicate.md)
            - [Schema rehearsal](schema-rehearsal.md)
            - [October capacity](october-capacity.md)
            - [Customer export](customer-export.md)
            - [Session incident](session-incident.md)
        """,
        "docs/exec-plans/active/index.md": """
            # Active execution plans

            - [Settlement cutover](settlement-cutover.md)
            - [Provider routing review](provider-routing-review.md)
        """,
        "docs/exec-plans/completed/index.md": """
            # Completed execution plans

            - [Order admission rollout](order-admission-rollout.md)
            - [Provider adapter rollout](provider-adapter-rollout.md)
        """,
        "docs/worklog/active/index.md": """
            # Active worklog

            - [Settlement handoff](2026-10-18-settlement.md)
            - [Queue review](2026-10-22-queue.md)
        """,
        "docs/worklog/archive/index.md": """
            # Archived worklog

            - [August provider rehearsal](2026-08-provider.md)
            - [September tenant audit](2026-09-tenant-audit.md)
        """,
        "docs/product-specs/index.md": """
            # Product specifications

            - [Checkout](checkout.md)
            - [Tenant billing](tenant-billing.md)
            - [Operations console](operations-console.md)
            - [Retention](retention.md)
        """,
        "docs/references/index.md": """
            # References

            - [API errors](api-errors.md)
            - [Status codes](status-codes.md)
            - [Glossary](glossary.md)
            - [Event examples](event-examples.md)
        """,
    }


def _topic_docs() -> dict[str, str]:
    return {
        "docs/design-docs/order-lifecycle.md": """
            # Order lifecycle

            An order moves from draft to accepted, authorized, captured, and
            finally settled. The transition record carries tenant and idempotency
            context so a replay cannot create a second charge.

            The gateway owns admission while domain transitions stay under
            src/domain/order.rs. Payment work is handed to the durable queue.
        """,
        "docs/design-docs/payment-routing.md": """
            # Payment routing

            Card authorization is sent to the primary provider; capture and refund
            use the adapter selected for the tenant region. A provider response is
            recorded before the order event is published.

            The adapter owns its HTTP retry budget, while queue redelivery has a
            separate policy. The capture decision describes the ordering.
        """,
        "docs/design-docs/tenant-isolation.md": """
            # Tenant boundaries

            Every storage key begins with the authenticated tenant_id. Admin views
            use an explicit support scope and never borrow a tenant from display
            input.

            The request context is checked at the gateway and again at repository
            boundaries. Tenant operations describe the incident checks.
        """,
        "docs/design-docs/webhook-delivery.md": """
            # Webhook delivery

            Order events are delivered at least once. Each destination receives a
            stable event id and may acknowledge after its own processing
            transaction; duplicate deliveries are expected and harmless.

            Delivery attempts use a bounded queue policy, while the HTTP client has
            a smaller transport budget.
        """,
        "docs/design-docs/queue.md": """
            # Queue behavior

            The durable work queue accepts a job when capacity is available. If
            admission finds the queue full, the API responds with HTTP 429 and
            includes the correlation id; workers continue draining existing jobs.

            Dependency outages also produce a 503 at the gateway, with a
            dependency-unavailable reason. Queue operations distinguishes that
            condition from admission capacity and worker lease age.
        """,
        "docs/design-docs/queue-v1.md": """
            ---
            status: superseded
            superseded_by: queue.md
            ---
            # Queue behavior

            The queue returns HTTP 429 when admission cannot accept a job.
            Clients receive the request correlation id and a Retry-After header;
            workers continue processing jobs already admitted to the queue.

            Worker retry timing is independent of the admission response.
        """,
        "docs/design-docs/retry-budgets.md": """
            # Retry budgets

            Transport retries protect a short network interruption; queue
            redelivery protects a worker crash. Neither budget changes an order
            state without an idempotency check.

            The HTTP adapter, provider adapter, and webhook worker each record
            attempts in their own metrics.
        """,
        "docs/design-docs/cache-layout.md": """
            # Cache layout

            Order-list entries use a tenant-prefixed namespace and a versioned key.
            Settlement exports share the namespace marker but use a day suffix so
            a late capture does not overwrite an order page.

            Cache invalidation is asynchronous; the cache observation records the
            last namespace migration review.
        """,
        "docs/design-docs/settlement-reconciliation.md": """
            # Settlement reconciliation

            The settlement job groups captured payments by tenant and provider day,
            then compares the ledger total with the provider export. A mismatch
            produces a review item rather than silently changing money.

            Reconciliation runs after the ledger snapshot and before an export is
            signed. The active cutover plan owns the rollout work.
        """,
        "docs/design-docs/observability.md": """
            # Observability

            Correlation ids span gateway admission, queue delivery, provider calls,
            and settlement rows. Metrics use tenant-safe labels and keep customer
            identifiers out of log fields.

            On-call dashboards cover the same boundaries under operations.
        """,
        "docs/decisions/000-idempotency.md": """
            ---
            status: accepted
            authority: canonical
            ---
            # Idempotency at order creation

            A tenant supplied idempotency key is stored before a new order is
            accepted. Replaying the same key returns the original outcome; a
            different payload with that key is rejected for review.

            The rule is implemented around src/domain/idempotency.rs.
        """,
        "docs/decisions/001-payment-capture.md": """
            ---
            status: accepted
            authority: canonical
            ---
            # Capture ordering

            A successful provider capture is written to the ledger before the
            order.captured event is published. A timeout remains pending until
            provider lookup resolves it.

            The adapter and queue boundaries are covered by payment routing.
        """,
        "docs/decisions/002-session-renewal.md": """
            ---
            status: accepted
            authority: canonical
            ---
            # Session renewal ownership

            Concurrent session renewals share one in-flight provider request. When
            that request fails, the renewal operation records the failure and
            returns it to its caller; the caller chooses what a later request
            should do. Keeping the decision at the request boundary prevents a
            storm of provider calls during an outage.

            src/auth/refresh.rs#refresh_token is the narrow renewal operation;
            src/auth/session.rs#renew_session is its direct coordinator.
        """,
        "docs/decisions/003-tenant-data.md": """
            ---
            status: accepted
            authority: canonical
            ---
            # Tenant data boundaries

            A repository method receives authenticated tenant context; callers may
            not substitute a tenant id parsed from display input. Support access
            is audited with an explicit scope.

            This decision backs the tenant boundary design and runbook.
        """,
        "docs/decisions/004-event-delivery.md": """
            ---
            status: accepted
            authority: canonical
            ---
            # Event delivery

            Events use at-least-once delivery. Consumers persist their event id
            before acknowledging work, so a redelivery is a harmless replay.

            Queue retry and HTTP retry remain separate budgets.
        """,
        "docs/decisions/005-money.md": """
            ---
            status: accepted
            authority: canonical
            ---
            # Money representation

            Prices and ledger amounts are integer minor units with an explicit
            currency code. Floating point values may appear in dashboards only
            after formatting a recorded integer amount.

            Invoice and settlement modules follow this rule.
        """,
        "docs/decisions/006-schema-evolution.md": """
            ---
            status: accepted
            authority: canonical
            ---
            # Schema evolution

            New event fields are optional for one release cycle. Readers accept
            the previous envelope version before writers switch their default.

            Migration notes stay with the affected design document.
        """,
        "docs/decisions/007-reconciliation.md": """
            ---
            status: accepted
            authority: canonical
            ---
            # Reconciliation exceptions

            A money mismatch becomes a review item with its provider reference; an
            operator resolves it only after ledger and provider rows are attached.

            Settlement operations defines the handoff.
        """,
        "docs/operations/gateway-throttles.md": """
            # Gateway throttles

            Updated: 2026-10-12

            The gateway allows 100 requests per minute for the standard tenant
            bucket. The tenant
            quota and database pool have separate limits and are not changed by
            this edge setting.

            When a bucket rejects traffic, capture the correlation id and tenant
            before comparing it with gateway metrics.
        """,
        "docs/operations/order-oncall.md": """
            # Order on-call

            Start with correlation id, tenant scope, and order state. A pending
            capture should be checked at the provider before asking a client to
            submit another request.

            The order lifecycle and replay procedure provide the escalation path.
        """,
        "docs/operations/payment-operations.md": """
            # Payment operations

            Provider outages are recorded by region and operation. Authorization,
            capture, and refund have different retry and reconciliation paths; do
            not replay a capture without its provider reference.

            Follow payment routing before changing a tenant provider assignment.
        """,
        "docs/operations/queue-operations.md": """
            # Queue operations

            Queue depth, oldest age, worker leases, and dependency health are read
            together. A saturated queue produces client back-pressure; a
            dependency outage is tracked separately in the gateway.

            Use queue behavior and event replay when deciding whether to drain or
            pause workers.
        """,
        "docs/operations/tenant-boundaries.md": """
            # Tenant boundaries

            Inspect authenticated tenant context before opening a support session.
            Any cross-tenant result is an incident even if the customer id looks
            valid.

            The tenant data decision is authoritative for repository and cache
            checks.
        """,
        "docs/operations/database-connections.md": """
            # Database connections

            The primary pool has a ceiling of 40 connections per service
            instance. Tenant quotas, gateway buckets, and provider limits are
            independent controls.

            During an incident compare pool wait time with queue age before
            increasing deployment size.
        """,
        "docs/operations/event-replay.md": """
            # Event replay

            Replay one event id at a time from the retained envelope. Confirm the
            consumer idempotency record and attach the result to the incident
            before acknowledging a manual replay.

            Webhook delivery describes the normal acknowledgement boundary.
        """,
        "docs/operations/release-process.md": """
            # Release process

            A release includes the binary, migration notes, and the smoke test
            report for affected tenant flows. The operator records the build
            revision before switching traffic.

            Schema changes are rehearsed against a copy of the settlement export.
        """,
        "docs/operations/cache-operations.md": """
            # Cache operations

            Warm a tenant namespace after deployment and compare an order list with
            its settlement sample. A stale entry can be evicted without changing
            the ledger source of truth.

            Cache layout describes key prefixes and the cache observation records
            the last migration review.
        """,
        "docs/operations/session-operations.md": """
            # Session operations

            Expired sessions are renewed at the request boundary. A failed renewal
            is surfaced with its correlation id so the next request can follow its
            client policy without making the renewal primitive loop.

            Read session renewal ownership before adjusting expiry or credentials.
        """,
        "docs/operations/settlement-operations.md": """
            # Settlement operations

            The daily batch is held for review when provider and ledger totals do
            not match. Operators attach both rows, record tenant scope, and wait
            for a second reviewer before releasing an export.

            Settlement design and the active cutover plan define the handoff.
        """,
        "docs/notes/cache-observation.md": """
            # Cache observation

            The October Northwind export contained USD minor units while older
            entries omitted the currency tag. Billing now publishes these rows
            under namespace v8; v7 rows remain readable during the rolling deploy.

            The order-list reader takes its namespace from
            `src/cache/keys.rs#CACHE_NAMESPACE_VERSION`; settlement summaries
            select theirs in `src/cache/settlement.rs#CACHE_NAMESPACE_VERSION`.
            Both readers must move together to v8 because they read the same batch
            of rows published by billing. The report tool must keep reading retained
            v7 exports for the historical finance audit.

            See [cache layout](../design-docs/cache-layout.md) for the key shape.
        """,
        "docs/notes/provider-outage.md": """
            # Provider outage review

            A July provider interruption produced pending captures but no ledger
            duplication. The team used provider references to reconcile pending
            rows before reopening traffic.

            The follow-up is captured in payment operations.
        """,
        "docs/notes/queue-load.md": """
            # Queue load sample

            The October load sample showed a long tail of leased jobs while the
            worker pool was healthy. Queue age and dependency status were more
            useful than a single depth number during triage.

            Compare the sample with queue operations.
        """,
        "docs/notes/tenant-support.md": """
            # Tenant support case

            A Cedar support case confirmed that the dashboard support scope was
            recorded separately from tenant id in the request. The audit row made
            the correction straightforward.

            See tenant boundaries.
        """,
        "docs/notes/settlement-handoff.md": """
            # Settlement handoff

            The previous shift replayed the ledger sample and left the provider
            export attached to the review case. The remaining handoff is tracked
            in the active settlement plan.
        """,
        "docs/notes/webhook-duplicate.md": """
            # Webhook duplicate

            A partner delivered the same event twice during a deploy. The event id
            record made the second delivery a no-op while the first transaction
            remained visible in the audit trail.

            Webhook design explains the acknowledgement sequence.
        """,
        "docs/notes/schema-rehearsal.md": """
            # Schema rehearsal

            The rehearsal accepted an older event envelope while the new writer
            emitted its optional field. The migration was completed before the
            release switch.

            See schema evolution.
        """,
        "docs/notes/october-capacity.md": """
            # October capacity

            Capacity planning separated edge traffic, tenant order volume, pool
            connections, and provider calls. The dashboards avoided treating one
            limit as a proxy for another.

            The links are collected under operations.
        """,
        "docs/notes/customer-export.md": """
            # Customer export

            The export job writes a tenant-scoped manifest before streaming order
            rows. A failed stream can resume from the manifest without changing
            the order ledger.

            Tenant billing owns the retention requirements.
        """,
        "docs/notes/session-incident.md": """
            # Session incident

            During a credential provider incident, several requests observed the
            same renewal failure. Correlation ids showed one provider request for
            the concurrent group rather than a request storm.

            The ownership boundary is in the session decision.
        """,
        "docs/exec-plans/active/settlement-cutover.md": """
            ---
            status: active
            ---
            # Settlement cutover

            The ledger sample has been replayed and the provider export is ready
            for the next batch rehearsal. The provider consolidates captures in
            90-second windows during the October cutover.

            - [x] Replay the ledger sample against the provider export.
            - [ ] Set the batch window in src/settlement/window.rs to 90 seconds.
            - [ ] Rehearse the export on the staging tenant.
        """,
        "docs/exec-plans/active/provider-routing-review.md": """
            ---
            status: superseded
            superseded_by: settlement-cutover.md
            ---
            # Provider routing review

            The capture adapter will try the regional provider when the primary
            provider is unavailable. Preserve the original provider reference in
            the review case so operations can trace either outcome.

            - [x] Collect primary-provider outage traces.
            - [ ] Add bounded capture retry in src/payments/provider.rs.
            - [ ] Reconcile the alternate provider export.
        """,
        "docs/exec-plans/completed/order-admission-rollout.md": """
            ---
            status: completed
            ---
            # Order admission rollout

            Tenant-scoped admission, idempotency storage, and correlation ids were
            rolled out in three stages. The final checks are retained for the next
            release rehearsal.
        """,
        "docs/exec-plans/completed/provider-adapter-rollout.md": """
            ---
            status: completed
            ---
            # Provider adapter rollout

            Primary and regional provider adapters record their reference before
            publishing a capture event. Settlement reconciliation owns provider
            mismatches.
        """,
        "docs/worklog/active/2026-10-18-settlement.md": """
            # 2026-10-18 settlement handoff

            Replayed the ledger sample and attached the Northwind provider export
            to the review case. The totals agree in minor units. No staging export
            has been attempted yet; the shift ended before the batch window change.

            Delivery checklist: [settlement cutover](../../exec-plans/active/settlement-cutover.md).
        """,
        "docs/worklog/active/2026-10-22-queue.md": """
            # 2026-10-22 queue review

            Worker lease age was sampled with queue depth and dependency health.
            The team left the current admission contract unchanged while the next
            API release is prepared.

            Queue operations contains the live triage sequence.
        """,
        "docs/worklog/archive/2026-08-provider.md": """
            # August provider rehearsal

            The provider reference was added to capture rows and replayed through
            the reconciliation worksheet. The rehearsal ended before a production
            traffic switch.
        """,
        "docs/worklog/archive/2026-09-tenant-audit.md": """
            # September tenant audit

            The audit sampled support scopes and tenant-prefixed cache keys. No
            cross-tenant result was found in the retained sample.
        """,
        "docs/product-specs/checkout.md": """
            # Checkout

            Checkout accepts a tenant-scoped cart, validates inventory, and stores
            an idempotency key before returning an order id. A client may safely
            resume after a network timeout.

            Payment capture remains asynchronous.
        """,
        "docs/product-specs/tenant-billing.md": """
            # Tenant billing

            Billing exports are partitioned by tenant and settlement day. A
            customer downloads a signed manifest without receiving another
            tenant's rows.

            The export handoff follows settlement reconciliation.
        """,
        "docs/product-specs/operations-console.md": """
            # Operations console

            Operators see correlation id, tenant scope, order state, and provider
            reference before an action is enabled. Manual replay requires a case
            reference and leaves an audit event.

            Order on-call owns the escalation path.
        """,
        "docs/product-specs/retention.md": """
            # Retention

            Order and payment events remain queryable for the retention window;
            exported customer manifests retain their checksum and tenant scope.

            Retention cleanup does not remove a ledger row needed for settlement.
        """,
        "docs/references/api-errors.md": """
            # API errors

            API errors include a correlation id and stable reason. Admission
            back-pressure, dependency outage, and validation failure are separate
            categories for clients and on-call staff.
        """,
        "docs/references/status-codes.md": """
            # Status codes

            The service uses 200 for reads, 202 for accepted work, 429 for
            tenant quota and gateway bucket rejection, and 503 for unavailable
            dependencies. Provider status is stored separately. Work queue
            admission follows the [queue contract](../design-docs/queue.md).
        """,
        "docs/references/glossary.md": """
            # Glossary

            A tenant is an isolated merchant account. Settlement is the batch
            comparison between the Harbourline ledger and a provider export.
            Correlation id connects one request to asynchronous work.
        """,
        "docs/references/event-examples.md": """
            # Event examples

            An order event carries tenant id, order id, event id, envelope version,
            and a provider reference when one exists. Consumers persist event id
            before acknowledging it.
        """,
    }


def _product_docs() -> dict[str, str]:
    files = _doc_indexes()
    files.update(_topic_docs())
    files.update({
        "README.md": """
            # Harbourline orders

            A tenant-scoped checkout, payment capture, and settlement service.
            Start at the [documentation index](docs/index.md) for service
            contracts and the [project map](docs/project.md) for delivery status.

            Run `cargo test` to exercise the checkout and worker flows. Local
            merchant settings live in config/tenants; API envelopes and examples
            live under api/. Money is represented in integer minor units.
        """,
        "AGENTS.md": """
            # Harbourline agent route

            Read [the architecture map](ARCHITECTURE.md), then use [the document
            index](docs/index.md) to find the smallest current source for a task.
            The Rust service is the behavior source; durable changes belong in the
            existing document that owns the contract. Run service tests after code
            changes.
        """,
        "ARCHITECTURE.md": """
            # Harbourline architecture

            Harbourline serves orders for independent tenants. Requests enter at
            src/gateway, cross domain modules, and send payment work through
            payments and queue. Settlement batches are assembled by settlement.

            The document map routes design choices, operating procedures, and
            handoff notes. Tools under tools support reconciliation and inspection.
        """,
        "Cargo.toml": """
            [package]
            name = "harbourline_order_service"
            version = "0.8.0"
            edition = "2021"

            [lib]
            path = "src/lib.rs"
        """,
        ".gitignore": "target/\n",
    })
    # Keep the corpus in the requested roughly sixty-document range while
    # retaining coverage in the remaining operational and reference pages.
    removed = (
        "docs/notes/customer-export.md",
        "docs/operations/session-operations.md",
        "docs/operations/settlement-operations.md",
        "docs/operations/cache-operations.md",
        "docs/product-specs/retention.md",
        "docs/product-specs/operations-console.md",
        "docs/references/event-examples.md",
        "docs/exec-plans/completed/provider-adapter-rollout.md",
        "docs/worklog/archive/2026-09-tenant-audit.md",
    )
    for path in removed:
        files.pop(path, None)
    replacements = {
        "docs/notes/index.md": ("            - [Customer export](customer-export.md)\n", ""),
        "docs/operations/index.md": (
            "            - [Cache operations](cache-operations.md)\n            - [Session operations](session-operations.md)\n            - [Settlement operations](settlement-operations.md)\n",
            "",
        ),
        "docs/product-specs/index.md": ("            - [Operations console](operations-console.md)\n            - [Retention](retention.md)\n", ""),
        "docs/references/index.md": ("            - [Event examples](event-examples.md)\n", ""),
        "docs/exec-plans/completed/index.md": ("            - [Provider adapter rollout](provider-adapter-rollout.md)\n", ""),
        "docs/worklog/archive/index.md": ("            - [September tenant audit](2026-09-tenant-audit.md)\n", ""),
        "docs/design-docs/cache-layout.md": ("            Cache invalidation is asynchronous; the cache observation records the\n            last namespace migration review.\n", "            Cache invalidation is asynchronous; the cache observation records the\n            last namespace migration review.\n"),
        "docs/notes/cache-observation.md": ("            Cache design and operations describe the recovery path.\n", "            Cache design describes the recovery path.\n"),
    }
    for path, (old, new) in replacements.items():
        files[path] = files[path].replace(old, new)
    # Focused documents link back to their collection and to a relevant service
    # contract. Index pages provide the broad route without copying that prose.
    for path in sorted(files):
        if path.startswith("docs/") and path.endswith(".md") and not path.endswith("/index.md"):
            if path in {"docs/index.md", "docs/project.md"}:
                continue
            files[path] = textwrap.dedent(files[path]).lstrip() + "\n" + "[Collection index](index.md).\n"
    return files


def _scattered_s6_files() -> dict[str, str]:
    files: dict[str, str] = {
        "AGENTS.md": """
            # Harbourline agent notes

            Read [ARCHITECTURE.md](ARCHITECTURE.md) for the service map. The
            merchant onboarding examples are in [README.md](README.md).
        """,
        "ARCHITECTURE.md": """
            # Harbourline architecture

            The Rust service handles tenant orders, payment capture, and worker
            jobs. The old handbook is in [README.md](README.md) and investigations
            are in [notes/index.md](notes/index.md).
        """,
        ".gitignore": "target/\n",
        "Cargo.toml": """
            [package]
            name = "harbourline_order_service"
            version = "0.8.0"
            edition = "2021"

            [lib]
            path = "src/lib.rs"
        """,
        "README.md": """
            # Harbourline service handbook

            Harbourline serves orders for independent merchants. The service is
            small enough to run as one Rust process, but its queue and payment
            adapters are separated so a provider incident cannot corrupt the
            order ledger.

            ## Checkout

            Order creation requires an idempotency key. The key is stored before a
            new order is accepted, and a replay returns the original outcome.
            Cart and inventory checks run before the payment request.

            ## Events

            The event bus uses at-least-once delivery. Consumers persist event ids
            before acknowledging a message, so duplicate delivery is an expected
            recovery path rather than a second business action.

            ## Tenant data

            Tenant data is isolated by tenant_id in request context, storage keys,
            cache entries, and export manifests. Support access carries a separate
            audited scope.

            ## Workers

            Failed background jobs enter a dead-letter queue after their delivery
            budget is exhausted. Operators attach the original correlation id and
            provider reference before replaying a job.

            ## Production

            Production releases require two-person review. One operator records the
            revision and migration result; a second reviewer confirms the smoke
            test and tenant-scoped export before traffic moves.

            ## Working notes

            The July [provider review](notes/provider-review.md) covers the
            capture outage. The [queue sample](notes/queue-sample.md) records the
            worker measurements. The [release checklist](ops/release.md) is used
            at the change window with the [deployment smoke steps](ops/smoke.md).
        """,
        "notes/index.md": """
            # Loose notes

            - [Provider review](provider-review.md)
            - [Order creation](order-creation.md)
            - [Queue sample](queue-sample.md)
            - [Old dashboard export](dashboard-export.md)
        """,
        "ops/release.md": """
            # Release checklist

            Production releases require two-person review. The release owner
            records the revision and migration result, and a second reviewer
            confirms smoke tests before traffic moves.
        """,
    }
    notes: list[tuple[str, str]] = [
        ("order-creation", "Order creation requires an idempotency key. The first write stores the key before acceptance, so a client retry returns the original outcome."),
        ("event-delivery", "The event bus uses at-least-once delivery. Consumers persist event ids before acknowledging work and treat a duplicate as a no-op."),
        ("tenant-storage", "Tenant data is isolated by tenant_id in request context, storage keys, cache entries, and export manifests. Support access carries an audited scope."),
        ("job-failures", "Failed background jobs enter a dead-letter queue after their delivery budget is exhausted. The original correlation id travels with a replay."),
        ("production-review", "Production releases require two-person review. A release owner records the revision and a second reviewer confirms smoke tests before traffic moves."),
        ("provider-review", "The July provider review found pending captures but no duplicate ledger rows. Provider references were attached before traffic reopened."),
        ("queue-sample", "The queue sample compared oldest age, worker leases, and dependency health instead of treating depth as the only signal."),
        ("cache-keys", "Order list and settlement export keys share a tenant prefix. The settlement suffix includes the business day used by reconciliation."),
        ("session-expiry", "Concurrent session expiry requests shared one provider operation during the credential incident. Correlation ids made the grouping visible."),
        ("support-scope", "Support sessions carry an explicit scope separate from the tenant id parsed from the dashboard URL. The audit row is retained."),
        ("inventory-window", "Inventory checks run before payment authorization and reserve a short window for the order transition."),
        ("refund-audit", "Refund requests include the provider reference and original order correlation id before a worker accepts them."),
        ("export-manifest", "Exports write a tenant-scoped manifest and checksum before streaming rows, which permits a failed stream to resume."),
        ("reconciliation", "Settlement compares the ledger total with the provider export and creates a review item for a mismatch."),
        ("correlation", "Correlation ids span gateway admission, queue delivery, provider calls, and settlement rows."),
        ("merchant-onboarding", "Merchant onboarding records region, currency, and provider adapter before the first order is accepted."),
        ("webhook-ack", "Webhook consumers persist an event id before acknowledging a partner delivery."),
        ("replay-window", "Manual replay is limited to one event id at a time and requires an incident reference."),
        ("ledger-snapshot", "The ledger snapshot is sealed before a settlement batch compares provider rows."),
        ("tenant-quota", "Gateway allowance, tenant daily quota, and database pool ceiling are independent controls."),
        ("database-pool", "Pool wait time is compared with queue age before an operator increases service capacity."),
        ("provider-timeout", "A provider timeout remains pending until a lookup resolves it; it is not immediately treated as a failed capture."),
        ("order-state", "Order state transitions are recorded with tenant and idempotency context."),
        ("fraud-check", "Fraud checks return a review decision without exposing customer identifiers in metrics."),
        ("shipping-adapter", "Shipping adapters report a stable carrier reference to the order timeline."),
        ("currency", "Money is stored as integer minor units with an explicit currency code."),
        ("tax-rounding", "Tax rounding is applied to integer minor units and recorded with the invoice."),
        ("line-items", "Line item snapshots preserve the catalog price used at checkout."),
        ("cart-expiry", "A cart expiry releases a reservation without deleting the order audit trail."),
        ("authorization", "Authorization and capture use separate provider references in the payment record."),
        ("capture-worker", "Capture workers lease one payment at a time and renew the lease before a slow provider response."),
        ("refund-worker", "Refund workers keep the original capture reference beside the refund outcome."),
        ("queue-leases", "Queue leases carry a correlation id and expiry timestamp for recovery."),
        ("dead-letter-review", "Dead-letter review groups failed jobs by cause before selecting a replay batch."),
        ("worker-drain", "Workers drain existing jobs before a planned deployment switches traffic."),
        ("gateway-errors", "Gateway errors return a stable reason and correlation id to clients."),
        ("rate-window", "Rate windows are measured per tenant bucket and independent of provider retry timing."),
        ("config-refresh", "Configuration refresh merges validated values and leaves the active snapshot immutable."),
        ("metrics-labels", "Metrics use tenant-safe labels and avoid customer identifiers in log fields."),
        ("trace-shape", "Tracing spans carry the order correlation id across asynchronous worker boundaries."),
        ("schema-rehearsal", "Schema rehearsal accepts an older event envelope while writers add optional fields."),
        ("migration-window", "Migration windows are scheduled before the release owner records the revision."),
        ("release-owner", "The release owner attaches migration output and smoke-test evidence to the change."),
        ("second-reviewer", "The second reviewer confirms tenant-scoped export evidence before traffic moves."),
        ("incident-links", "Incident notes link provider references, correlation ids, and affected tenant scope."),
        ("retention", "Retention cleanup keeps the ledger row required for later settlement reconciliation."),
        ("customer-manifest", "Customer manifests retain a checksum and tenant scope for the retention window."),
        ("admin-console", "The admin console enables manual action only after showing order state and provider reference."),
        ("region-routing", "Region routing selects a provider adapter before the first authorization request."),
        ("retry-boundaries", "Transport retries, queue redelivery, and provider lookup each have independent attempt records."),
        ("health-check", "Health checks distinguish process readiness from dependency availability."),
        ("backup-restore", "Restore rehearsals verify tenant prefixes and idempotency records before opening a read path."),
        ("launch-window", "Launch windows include a quiet period for settlement export review."),
    ]
    for stem, paragraph in notes:
        files[f"notes/{stem}.md"] = f"# {stem.replace('-', ' ').title()}\n\n{paragraph}\n"
    # The handbook accumulated service-owner worksheets before there was a docs
    # tree. Its repeated procedures and dead links are S6's initial condition.
    files["README.md"] = textwrap.dedent(files["README.md"]).lstrip()
    for stem, paragraph in notes[5:40]:
        files["README.md"] += f"\n## {stem.replace('-', ' ').title()}\n\n{paragraph}\n\nOwner notes: [worksheet](notes/{stem}.md).\n"
    return files


def _schema_old() -> str:
    fixture = ROOT.parent.parent / "tests/fixtures/compat/alpha6-schema.yaml"
    if fixture.is_file():
        return fixture.read_text(encoding="utf-8")
    return "schema_version: 0\nmanaged_root: docs\nentrypoints:\n  agents: AGENTS.md\n"


def _schema_new() -> str:
    raw = _schema_old().replace("schema_version: 0", "schema_version: 1", 1)
    def fields(match: re.Match) -> str:
        names = [name.strip() for name in match.group(1).split(",") if name.strip()]
        for name in ("status", "superseded_by", "supersedes_parts_of"):
            if name not in names:
                names.append(name)
        return "allowed_frontmatter: [" + ", ".join(names) + "]"
    raw = re.sub(r"allowed_frontmatter: \[([^\]]*)\]", fields, raw)
    raw = raw.replace("rules:\n", "rules:\n  entrypoint_max_lines: 120\n  stale_active_plan_days: 30\n")
    return raw + "\ninclude:\n  docs/notes/**/*.md: notes\n"


def _old_skill() -> str:
    return (ROOT / "fixtures/alpha7-skill/SKILL.md").read_text(encoding="utf-8")


def _common_files(scenario_id: str) -> dict[str, str]:
    files = _product_docs()
    files.update(_rust_files())
    files.update(_rust_tools())
    files.update(_fixture_support())
    if scenario_id == "S7":
        files["CLAUDE.md"] = "See AGENTS.md for the repository route.\n"
    return files


def _s6_files() -> dict[str, str]:
    files = _scattered_s6_files()
    files.update(_rust_files())
    files.update(_rust_tools())
    files.update(_fixture_support())
    return files


def _append_update(root: Path, relative: str, paragraph: str) -> None:
    path = root / relative
    path.write_text(path.read_text(encoding="utf-8").rstrip() + "\n\n" + paragraph.strip() + "\n", encoding="utf-8")


def _git(root: Path, *args: str, env: dict[str, str] | None = None) -> None:
    command_env = os.environ.copy()
    if env:
        command_env.update(env)
    subprocess.run(["git", "-C", str(root), *args], env=command_env, check=True, capture_output=True, text=True)


def _commit(root: Path, message: str, commit_date: date, *, env: dict[str, str] | None = None) -> None:
    timestamp = commit_date.isoformat() + "T00:00:00+0000"
    env = {
        **(env or {}),
        "GIT_AUTHOR_NAME": "Harbourline Platform Team",
        "GIT_AUTHOR_EMAIL": "harbourline-e1@example.invalid",
        "GIT_COMMITTER_NAME": "Harbourline Platform Team",
        "GIT_COMMITTER_EMAIL": "harbourline-e1@example.invalid",
        "GIT_AUTHOR_DATE": timestamp,
        "GIT_COMMITTER_DATE": timestamp,
    }
    _git(root, "add", "-A", env=env)
    _git(root, "commit", "--quiet", "--no-gpg-sign", "-m", message, env=env)


def _history_updates(scenario_id: str) -> list[tuple[str, str, str]]:
    from history_fixture import history_updates

    return history_updates(scenario_id == "S6")


def _make_local_release(root: Path) -> None:
    """Package real release bits, with stable tar/gzip metadata, for offline use."""
    import gzip
    import tempfile

    repository = ROOT.parent.parent
    binary = repository / "target/release/harnesskit"
    if not binary.is_file():
        subprocess.run(["cargo", "build", "--release"], cwd=repository, check=True, timeout=900)
    version = subprocess.check_output([str(binary), "--version"], text=True).strip()
    if version != "harnesskit 0.2.0":
        raise RuntimeError(f"S7 requires the current real 0.2.0 binary, got {version}")
    release = root / "release"
    release.mkdir(parents=True, exist_ok=True)
    archive = release / "harnesskit-x86_64-unknown-linux-gnu.tar.gz"
    with tempfile.TemporaryDirectory(prefix="e1-release-") as temporary:
        package = Path(temporary) / "harnesskit-x86_64-unknown-linux-gnu"
        package.mkdir()
        shutil.copy2(binary, package / "harnesskit")
        shutil.copytree(repository / "agent-surfaces", package / "agent-surfaces")
        (package / "VERSION").write_text("0.2.0\n", encoding="utf-8")

        def stable_metadata(info: tarfile.TarInfo) -> tarfile.TarInfo:
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            info.mtime = 1_792_800_000
            return info

        with archive.open("wb") as output:
            with gzip.GzipFile(filename="", mode="wb", fileobj=output, mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w") as handle:
                    handle.add(package, arcname=package.name, filter=stable_metadata)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (release / "SHA256SUMS").write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
    shutil.copy2(repository / "install.sh", release / "install.sh")
    _write(root, "release/README.md", """
        # Workstation release cache

        The 0.2.0 Linux release and SHA256SUMS are cached here for workstations
        without network access. The standard installer accepts a file:// asset
        base URL. The workstation install prefix is HARNESSKIT_INSTALL_PREFIX;
        Codex discovers its installed skills beneath CODEX_HOME/skills.

        Use install.sh --help for installation options. Project schema and
        documentation are kept separately from workstation tool installation.
    """)
    _append_update(root, "README.md", "Local workstation packages are cached under [release](release/README.md).")


def adopt_harnesskit(root: Path, scenario_id: str, binary: Path, *, env: dict[str, str]) -> None:
    """Add B's tracked adaptation after the shared, tool-free product history."""

    _write(root, ".harnesskit/schema.yaml", _schema_old() if scenario_id == "S7" else _schema_new())
    # S6 has no resolved code anchors yet; keep an empty, valid tracked lock
    # rather than inventing an anchor in its product documentation.
    _write(root, ".harnesskit/knowledge.lock",
           "# HarnessKit knowledge.lock: confirmed document anchors and code fingerprints.\n")
    _append_update(root, "AGENTS.md", "This repository uses HarnessKit for documentation; see the HarnessKit skill.")
    ignored = root / ".gitignore"
    ignored.write_text(ignored.read_text(encoding="utf-8") + (
        ".harnesskit/state/\n.harnesskit/history/\n.harnesskit/index.lock\n"
    ), encoding="utf-8")
    if scenario_id == "S7":
        _write(root, "agent-surfaces/skills/harnesskit/SKILL.md", _old_skill())
        _write(root, "agent-surfaces/skills/harnesskit/references/cli.md",
               (ROOT / "fixtures/alpha7-skill/references/cli.md").read_text(encoding="utf-8"))
        # The cache is disposable; its product README route is tracked in this
        # same adaptation commit, leaving the initial workspace clean.
        with ignored.open("a", encoding="utf-8") as handle:
            handle.write("release/\n")
        _make_local_release(root)
    result = subprocess.run(
        [str(binary), "index", str(root)], cwd=root, env=env,
        text=True, capture_output=True, timeout=120, check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"B HarnessKit index failed: {result.stderr[-1000:]}")
    _commit(root, "Adopt HarnessKit for documentation", date(2026, 10, 23), env=env)


def generate_repo(scenario_id: str, destination: Path, *, force: bool = False) -> Path:
    """Create the shared Harbourline history without HarnessKit artifacts."""

    scenario_id = scenario_id.upper()
    if scenario_id not in SCENARIOS:
        raise ValueError(f"unknown scenario: {scenario_id}")
    if destination.exists():
        if not force:
            raise FileExistsError(f"destination exists: {destination}")
        shutil.rmtree(destination)
    destination.mkdir(parents=True)
    files = _s6_files() if scenario_id == "S6" else _common_files(scenario_id)
    _git(destination, "init", "--quiet", "-b", "main")
    _git(destination, "config", "user.name", "Harbourline Platform Team")
    _git(destination, "config", "user.email", "harbourline-e1@example.invalid")
    # The archived August rehearsal belongs in August; later dated observations
    # enter the history on their recorded day instead of appearing at bootstrap.
    bootstrap_date = date(2026, 8, 31)
    dated_docs: dict[str, tuple[date, str]] = {}
    for relative in sorted(files):
        content = re.sub(r"\]\s*\n\s*\(", "](", files[relative])
        if relative.endswith(".md"):
            recorded = re.search(r"\d{4}-\d{2}-\d{2}", Path(relative).name) or re.search(
                r"(?m)^\s*Updated:\s*(\d{4}-\d{2}-\d{2})", content
            )
            if recorded:
                recorded_on = date.fromisoformat(recorded.group(recorded.lastindex or 0))
                if recorded_on > bootstrap_date:
                    dated_docs[relative] = (recorded_on, content)
                    continue
        _write(destination, relative, content)
    _commit(destination, "Bootstrap merchant order and payment service", bootstrap_date)
    for day, (message, relative, paragraph) in enumerate(_history_updates(scenario_id), start=2):
        # End on October 22 so B's October 23 adoption predates scenario today.
        commit_date = date(2026, 9, 8) + timedelta(days=day - 1)
        for path, (recorded_on, content) in list(dated_docs.items()):
            if recorded_on <= commit_date:
                _write(destination, path, content)
                del dated_docs[path]
        if day == 38 and scenario_id != "S6":
            path = destination / "src/gateway/rate_limit.rs"
            path.write_text(path.read_text(encoding="utf-8").replace("= 100;", "= 120;", 1), encoding="utf-8")
            _commit(destination, "Raise gateway allowance after traffic review", commit_date)
            continue
        if day == 40 and scenario_id != "S6":
            _append_update(destination, "docs/exec-plans/active/settlement-cutover.md",
                           "The ledger rehearsal evidence is attached to SET-63; Northwind staging is reserved for the next operator shift.")
            _commit(destination, "Record settlement window handoff", commit_date)
            continue
        if day == 42 and scenario_id != "S6":
            _append_update(
                destination,
                "docs/exec-plans/active/provider-routing-review.md",
                "The regional outage traces are attached to PAY-217; review participants added the provider export checksum to the case.",
            )
            _commit(destination, "Attach regional outage traces to routing review", commit_date)
            continue
        _append_update(destination, relative, paragraph)
        _commit(destination, message, commit_date)
    if dated_docs:
        raise ValueError(f"dated documents fall outside the fixture history: {sorted(dated_docs)}")
    return destination


def _readme(spec: Scenario) -> str:
    arm_line = "A、B" if spec.arms == ("A", "B") else "仅 B"
    key_lines = "\n".join(f"- {path}" for path in spec.key_files)
    rules = {
        "S1": "刷新器及直接协调器无新增重试或退避且隐藏测试通过即成功；允许请求边界有界重试。答复引用决策、改写 accepted 决策正文仅记为指标。HTTP 客户端已有的独立重试不计入该路径。",
        "S2": "正确解让订单缓存与结算缓存一起迁移，并保留历史报表工具的 v7；只改一个消费者或一律改为 v8 都会失败。",
        "S3": "正确解执行现行计划的 90 秒窗口步骤，不推进旧计划或其代码方向；允许归档旧计划或调整其 frontmatter。计划勾选和任意 worklog 交接条目只记为过程指标。",
        "S4": "正确解在最终答复报告代码当前值，并更新运维说明或指出旧值已漂移；搜索到的其他限流数字不能替代网关值。",
        "S5": "正确解改现行 queue.md 和队列实现，保留 queue-v1 正文及索引，不创建重复设计文档；允许 worklog/changelog/notes 记录变更，隐藏测试验证 503。",
        "S6": "正确解修改 AGENTS 并保持不超过 120 行；关键事实经链接可达、无重复段落或失效链接、隐藏测试通过，不规定文档目录或文件名。",
        "S7": "正确解离线迁移 schema，更新 CODEX_HOME 中实际安装的 skill 与 references，保留产品文档正文，并通过 check；只有 B 组运行。",
    }[spec.scenario_id]
    return f"""# {spec.scenario_id}：{spec.title}

## 场景

- 实验组：{arm_line}
- Prompt（A/B 逐字相同）：{spec.prompt}
- 考点：{spec.focus}

## 关键文件

{key_lines}

## Grader 判定

{rules}

grader 以场景初始快照为基线，只检查相对 diff 和隐藏 cargo 测试；被试看不到隐藏测试。
"""


def write_definitions(destination: Path | None = None) -> Path:
    destination = destination or SCENARIO_ROOT
    destination.mkdir(parents=True, exist_ok=True)
    wrapper = """from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from grader_support import grade as _grade  # noqa: E402

SCENARIO = Path(__file__).resolve().parent.name


def grade(repo: Path, *, last_message: str = "", baseline=None, **kwargs):
    return _grade(SCENARIO, repo, last_message=last_message, baseline=baseline, **kwargs)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("repo", type=Path)
    parser.add_argument("--last-message", default="")
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--codex-home", type=Path)
    parser.add_argument("--harnesskit-binary")
    args = parser.parse_args()
    baseline = json.loads(args.baseline.read_text()) if args.baseline else None
    print(json.dumps(grade(args.repo, last_message=args.last_message, baseline=baseline,
                           codex_home=args.codex_home, harnesskit_binary=args.harnesskit_binary).to_dict(), ensure_ascii=False, indent=2))
"""
    for scenario_id, spec in SCENARIOS.items():
        folder = destination / scenario_id
        folder.mkdir(parents=True, exist_ok=True)
        _write(folder, "prompt.txt", spec.prompt + "\n")
        _write(folder, "README.md", _readme(spec))
        _write(folder, "grader.py", wrapper)
    return destination


def _replace_once(path: Path, old: str, new: str) -> None:
    text = path.read_text(encoding="utf-8")
    if old not in text:
        raise RuntimeError(f"recorded solution could not find {old!r} in {path}")
    path.write_text(text.replace(old, new, 1), encoding="utf-8")


def _clean_s6(root: Path) -> None:
    _write(root, "AGENTS.md", """
        # Harbourline agent route

        Read [ARCHITECTURE.md](ARCHITECTURE.md), then start at [docs/index.md](docs/index.md). Keep durable product
        facts in the focused document that owns them and keep this file as a
        route table.
    """)
    _write(root, "ARCHITECTURE.md", """
        # Harbourline architecture

        The Rust service handles tenant orders, payment capture, and workers.
        The durable map is docs/index.md; loose notes remain linked from its
        references page after cleanup.
    """)
    _write(root, "docs/index.md", """
        # Harbourline documentation

        Read the project map, then the operating invariants and release guide.
        Field notes preserve useful context from the former flat repository.
    """)
    # The explicit links are kept separate from the prose above so the route is
    # easy to inspect while the fact text has one canonical home.
    _replace_once(root / "docs/index.md", "the project map", "[project map](project.md)")
    _replace_once(root / "docs/index.md", "the operating invariants", "[operating invariants](design/order-invariants.md)")
    _replace_once(root / "docs/index.md", "release guide", "[release guide](operations/release.md)")
    _replace_once(root / "docs/index.md", "Field notes", "[field notes](references/field-notes.md)")
    _write(root, "docs/project.md", """
        # Project map

        Harbourline serves tenant-scoped orders and asynchronous payments. The
        focused documents below capture the invariants every service change must
        preserve.

        See the documentation route at index.md and the field-note index for
        historical context.
    """)
    _replace_once(root / "docs/project.md", "documentation route at index.md", "documentation route at [index.md](index.md)")
    _replace_once(root / "docs/project.md", "field-note index", "[field-note index](references/field-notes.md)")
    _write(root, "docs/design/order-invariants.md", """
        # Order invariants

        Order creation requires an idempotency key stored before acceptance;
        replaying the same key returns the original outcome. The event bus uses
        at-least-once delivery, so consumers
        persist event ids before acknowledging work.

        Tenant data is isolated by tenant_id in request context, storage keys,
        cache entries, and export manifests. Background jobs that fail enter a
        dead-letter queue after their delivery budget is exhausted.

        Support access carries a separate audited scope. A manual job replay
        retains its original correlation id and provider reference.
    """)
    _write(root, "docs/operations/release.md", """
        # Production release

        Production releases require two-person review. The release owner records
        the revision and migration result; a second reviewer confirms smoke tests
        and tenant-scoped export evidence before traffic moves.

        This guide is the canonical home for the release gate. The original
        checklist remains in the field-note index for historical context.
    """)
    note_paths = sorted(
        path.relative_to(root).as_posix()
        for path in (root / "notes").glob("*.md")
        if path.name != "index.md"
    )
    lines = [
        "# Field notes",
        "",
        "The cleaned notes retain incident context without copying the canonical invariant paragraphs.",
        "",
    ]
    for relative in note_paths:
        lines.append(f"- [{Path(relative).stem.replace('-', ' ').title()}](../../{relative})")
        note = root / relative
        title = Path(relative).stem.replace("-", " ").title()
        paragraphs = note.read_text(encoding="utf-8").strip().split("\n\n")[1:]
        if note.stem in S6_FACT_NOTES:
            paragraphs = paragraphs[1:]
        route = "See [operating invariants](../docs/design/order-invariants.md)."
        if note.stem == "production-review":
            route = "See [production release](../docs/operations/release.md)."
        note.write_text(f"# {title}\n\n" + "\n\n".join([*paragraphs, route]) + "\n", encoding="utf-8")
    _write(root, "notes/index.md", "# Field-note route\n\nSee [the maintained field-note index](../docs/references/field-notes.md).\n")
    _write(root, "docs/references/field-notes.md", "\n".join(lines) + "\n")
    _write(root, "ops/release.md", "# Historical release note\n\nThe old checklist points to [the release guide](../docs/operations/release.md).\n")
    _write(root, "README.md", "# Harbourline handbook\n\nThe maintained service facts now live under [docs/index.md](docs/index.md); this file retains the former handbook route.\n")


def apply_recorded_solution(
    scenario_id: str,
    root: Path,
    *,
    codex_home: Path | None = None,
    install_prefix: Path | None = None,
    env: dict[str, str] | None = None,
) -> str:
    if scenario_id == "S1":
        return (
            "我读了 docs/decisions/002-session-renewal.md：续期操作采用单飞，失败由调用方决定后续动作，"
            "刷新路径不应内部重试。当前没有改 refresh_token；建议在请求调用方增加有界且带抖动的重试，"
            "并继续让刷新器只负责一次 provider 请求。"
        )
    if scenario_id == "S2":
        _replace_once(root / "src/cache/keys.rs", "CACHE_NAMESPACE_VERSION: u32 = 7", "CACHE_NAMESPACE_VERSION: u32 = 8")
        _replace_once(root / "src/cache/settlement.rs", "CACHE_NAMESPACE_VERSION: u32 = 7", "CACHE_NAMESPACE_VERSION: u32 = 8")
        return "订单列表和结算摘要已一起迁移缓存命名空间版本，并运行了测试。"
    if scenario_id == "S3":
        _replace_once(root / "src/settlement/window.rs", "SETTLEMENT_WINDOW_SECONDS: u32 = 60", "SETTLEMENT_WINDOW_SECONDS: u32 = 90")
        _replace_once(root / "docs/exec-plans/active/settlement-cutover.md", "- [ ] Set the batch window", "- [x] Set the batch window")
        _write(root, "tests/batch_windows.rs", """
            use harbourline_order_service::settlement::window::{batch_bucket, settlement_window_seconds};

            #[test]
            fn cutover_window_groups_captures_on_ninety_second_boundaries() {
                assert_eq!(settlement_window_seconds(), 90);
                assert_eq!(batch_bucket(179), 1);
                assert_eq!(batch_bucket(180), 2);
            }
        """)
        _append_update(root, "docs/worklog/active/2026-10-18-settlement.md", "The settlement window is now 90 seconds and covered by tests/batch_windows.rs; the staging export rehearsal remains next.")
        return "我按 active settlement-cutover 计划完成了结算窗口步骤，并把结果写入 worklog；superseded 计划未执行。"
    if scenario_id == "S4":
        _replace_once(root / "docs/operations/gateway-throttles.md", "100 requests per minute", "120 requests per minute")
        return "当前网关标准租户限流值是 120 requests per minute；运维说明已按代码当前值更新。"
    if scenario_id == "S5":
        _replace_once(root / "src/queue/admission.rs", "\n    429\n", "\n    503\n")
        _replace_once(root / "docs/design-docs/queue.md", "HTTP 429", "HTTP 503")
        return "队列满载现在返回 503，现行 queue.md 已同步，queue-v1 正文和索引保持可追溯。"
    if scenario_id == "S6":
        _clean_s6(root)
        return "已从 AGENTS 建立文档入口，整理关键不变量、发布流程与可追溯的现场笔记，并去掉重复段落。"
    if scenario_id == "S7":
        import tempfile

        # Exercise the actual local installer and migration CLI. Do not simulate
        # installation by copying a version string into the subject's skill.
        with tempfile.TemporaryDirectory(prefix="e1-install-") as temporary:
            home = codex_home or Path(temporary) / "codex-home"
            prefix = install_prefix or Path(temporary) / "prefix"
            home.mkdir(parents=True, exist_ok=True)
            env = dict(env if env is not None else os.environ)
            env.update(CODEX_HOME=str(home), HARNESSKIT_USAGE_LOG="off")
            subprocess.run([
                "bash", str(root / "release/install.sh"), "--version", "0.2.0",
                "--asset-base-url", (root / "release").as_uri(),
                "--prefix", str(prefix), "--no-claude",
            ], env=env, cwd=root, check=True, capture_output=True, text=True, timeout=180)
            binary = prefix / "bin/harnesskit"
            subprocess.run([str(binary), "migrate", str(root), "--apply"],
                           env=env, cwd=root, check=True, capture_output=True, text=True, timeout=180)
        return "已用本地 0.2.0 release 完成 schema 迁移，并同步 CODEX_HOME skill 与 references。"
    raise ValueError(f"unknown scenario: {scenario_id}")


