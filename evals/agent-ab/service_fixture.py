"""Small dependency-free Harbourline service used by every E1 repository.

These modules implement the checkout, capture and settlement boundaries described
in the fixture docs. Keeping the source here also keeps private grading tests out
of the repository shown to the subject.
"""

from __future__ import annotations


def rust_files() -> dict[str, str]:
    files = {
        "src/domain/tenant.rs": """
            #[derive(Clone, Debug)]
            pub struct Tenant { pub id: String, pub currency: String }

            impl Tenant {
                pub fn new(id: &str, currency: &str) -> Result<Self, String> {
                    if id.is_empty() { return Err("missing tenant".into()); }
                    Ok(Self { id: id.into(), currency: currency.into() })
                }
            }
        """,
        "src/domain/customer.rs": """
            use super::tenant::Tenant;

            pub fn scoped_customer(tenant: &Tenant, customer: &str) -> Result<String, String> {
                if customer.is_empty() { return Err("missing customer".into()); }
                Ok(format!("{}:{customer}", tenant.id))
            }
        """,
        "src/domain/address.rs": """
            pub fn validate_country(country: &str) -> Result<(), String> {
                if country.len() == 2 && country.bytes().all(|b| b.is_ascii_uppercase()) {
                    Ok(())
                } else { Err("country must be an ISO alpha-2 code".into()) }
            }
        """,
        "src/domain/catalog.rs": """
            use super::currency::Money;

            pub fn unit_price(sku: &str, currency: &str) -> Result<Money, String> {
                let cents = match sku { "harbour-mug" => 1800, "canvas-bag" => 2500,
                    _ => return Err("unknown sku".into()) };
                Money::new(cents, currency)
            }
        """,
        "src/domain/cart.rs": """
            use super::{catalog, line_item::LineItem};

            pub fn price_cart(skus: &[(&str, u32)], currency: &str) -> Result<Vec<LineItem>, String> {
                if skus.is_empty() { return Err("empty cart".into()); }
                skus.iter().map(|(sku, quantity)| {
                    LineItem::new(sku, *quantity, catalog::unit_price(sku, currency)?)
                }).collect()
            }
        """,
        "src/domain/line_item.rs": """
            use super::currency::Money;

            #[derive(Clone, Debug)]
            pub struct LineItem { pub sku: String, pub quantity: u32, pub unit_price: Money }

            impl LineItem {
                pub fn new(sku: &str, quantity: u32, unit_price: Money) -> Result<Self, String> {
                    if quantity == 0 { return Err("zero quantity".into()); }
                    Ok(Self { sku: sku.into(), quantity, unit_price })
                }
                pub fn subtotal(&self) -> u64 { self.unit_price.cents * u64::from(self.quantity) }
            }
        """,
        "src/domain/currency.rs": """
            #[derive(Clone, Debug)]
            pub struct Money { pub cents: u64, pub currency: String }

            impl Money {
                pub fn new(cents: u64, currency: &str) -> Result<Self, String> {
                    if !matches!(currency, "USD" | "EUR" | "GBP" | "CAD" | "AUD" | "JPY") {
                        return Err("unsupported currency".into());
                    }
                    Ok(Self { cents, currency: currency.into() })
                }
            }
        """,
        "src/domain/tax.rs": """
            pub fn tax_cents(subtotal: u64, basis_points: u32) -> u64 {
                (subtotal * u64::from(basis_points) + 5000) / 10_000
            }
        """,
        "src/domain/invoice.rs": """
            use super::{line_item::LineItem, tax};

            pub fn total_cents(lines: &[LineItem], basis_points: u32) -> u64 {
                let subtotal: u64 = lines.iter().map(LineItem::subtotal).sum();
                subtotal + tax::tax_cents(subtotal, basis_points)
            }
        """,
        "src/domain/idempotency.rs": """
            use std::collections::BTreeMap;

            #[derive(Default)]
            pub struct IdempotencyStore { outcomes: BTreeMap<(String, String), String> }

            impl IdempotencyStore {
                pub fn get(&self, tenant: &str, key: &str) -> Option<&String> {
                    self.outcomes.get(&(tenant.into(), key.into()))
                }
                pub fn record(&mut self, tenant: &str, key: &str, outcome: &str) -> Result<(), String> {
                    if key.is_empty() { return Err("missing idempotency key".into()); }
                    let entry = self.outcomes.entry((tenant.into(), key.into())).or_insert_with(|| outcome.into());
                    if entry != outcome { return Err("idempotency conflict".into()); }
                    Ok(())
                }
            }
        """,
        "src/domain/metadata.rs": """
            pub const EVENT_ENVELOPE_VERSION: u16 = 4;

            pub fn correlation_id(tenant: &str, order: &str) -> String {
                format!("{tenant}/{order}")
            }
        """,
        "src/domain/order.rs": """
            use super::{cart, invoice, line_item::LineItem, tenant::Tenant};

            #[derive(Clone, Debug, PartialEq)]
            pub enum OrderState { Accepted, Captured, Settled }

            #[derive(Clone, Debug)]
            pub struct Order {
                pub id: String, pub tenant: Tenant, pub lines: Vec<LineItem>,
                pub total_cents: u64, pub state: OrderState,
            }

            pub fn create(tenant: Tenant, id: &str, skus: &[(&str, u32)]) -> Result<Order, String> {
                let lines = cart::price_cart(skus, &tenant.currency)?;
                let total_cents = invoice::total_cents(&lines, 800);
                Ok(Order { id: id.into(), tenant, lines, total_cents, state: OrderState::Accepted })
            }
        """,
        "src/auth/credentials.rs": """
            pub fn validate(credential: &str) -> Result<(), String> {
                if credential.is_empty() { Err("missing credential".into()) } else { Ok(()) }
            }
        """,
        "src/auth/claims.rs": """
            use crate::domain::tenant::Tenant;

            pub fn tenant_claim(tenant_id: &str) -> Result<Tenant, String> {
                Tenant::new(tenant_id, "USD")
            }
        """,
        "src/auth/refresh.rs": """
            use super::credentials;

            #[derive(Default)]
            pub struct RefreshState {
                pub in_flight: bool,
                pub provider_calls: u32,
                pub failures_remaining: u32,
            }

            pub fn refresh_token(state: &mut RefreshState, credential: &str) -> Result<String, String> {
                if state.in_flight { return Err("renewal already in flight".into()); }
                credentials::validate(credential)?;
                state.in_flight = true;
                state.provider_calls += 1;
                let result = if state.failures_remaining > 0 {
                    state.failures_remaining -= 1;
                    Err("credential provider unavailable".into())
                } else { Ok(format!("session:{credential}")) };
                state.in_flight = false;
                result
            }
        """,
        "src/auth/session.rs": """
            use super::refresh::{refresh_token, RefreshState};

            pub fn renew_session(state: &mut RefreshState, credential: &str) -> Result<String, String> {
                refresh_token(state, credential)
            }
        """,
        "src/cache/entries.rs": """
            use std::collections::BTreeMap;

            #[derive(Default)]
            pub struct Entries { values: BTreeMap<String, String> }

            impl Entries {
                pub fn put(&mut self, key: String, value: String) { self.values.insert(key, value); }
                pub fn get(&self, key: &str) -> Option<&String> { self.values.get(key) }
                pub fn refresh(&mut self, key: String, value: String) { self.put(key, value); }
                pub fn invalidate(&mut self, prefix: &str) { self.values.retain(|k, _| !k.starts_with(prefix)); }
            }
        """,
        "src/cache/invalidation.rs": """
            use super::{entries::Entries, keys::namespace_key};

            pub fn invalidate_order(entries: &mut Entries, tenant: &str, order: &str) {
                entries.invalidate(&namespace_key(tenant, order));
            }
        """,
        "src/cache/keys.rs": """
            pub const CACHE_NAMESPACE_VERSION: u32 = 7;

            pub fn namespace_key(tenant_id: &str, order_id: &str) -> String {
                format!("orders:v{}:{tenant_id}:{order_id}", CACHE_NAMESPACE_VERSION)
            }
        """,
        "src/cache/settlement.rs": """
            pub const CACHE_NAMESPACE_VERSION: u32 = 7;

            pub fn settlement_key(tenant_id: &str, day: &str) -> String {
                format!("settlement:v{}:{tenant_id}:{day}", CACHE_NAMESPACE_VERSION)
            }

            pub fn refresh_summary(entries: &mut super::entries::Entries, tenant: &str, day: &str, cents: u64) {
                entries.refresh(settlement_key(tenant, day), cents.to_string());
            }
        """,
        "src/gateway/rate_limit.rs": """
            pub const GATEWAY_REQUESTS_PER_MINUTE: u32 = 100;

            pub fn allowed(requests: u32) -> bool { requests <= GATEWAY_REQUESTS_PER_MINUTE }
        """,
        "src/gateway/tenant_quota.rs": """
            pub const TENANT_DAILY_ORDER_QUOTA: u32 = 600;

            pub fn within_quota(orders: u32) -> bool { orders <= TENANT_DAILY_ORDER_QUOTA }
        """,
        "src/gateway/session_access.rs": """
            use crate::auth::{refresh::RefreshState, session::renew_session};

            pub fn request_session(state: &mut RefreshState, credential: &str) -> Result<String, String> {
                renew_session(state, credential)
            }
        """,
        "src/gateway/ingress.rs": """
            use crate::{auth::claims, domain::{idempotency::IdempotencyStore, order::{self, Order}}, queue::admission};
            use super::{rate_limit, tenant_quota};

            pub fn submit(tenant_id: &str, key: &str, orders_today: u32, requests: u32,
                          queue_depth: usize, store: &mut IdempotencyStore) -> Result<Order, u16> {
                if !rate_limit::allowed(requests) || !tenant_quota::within_quota(orders_today) { return Err(429); }
                admission::admit(queue_depth)?;
                let tenant = claims::tenant_claim(tenant_id).map_err(|_| 401u16)?;
                let id = format!("order-{key}");
                store.record(tenant_id, key, &id).map_err(|_| 409u16)?;
                order::create(tenant, &id, &[("harbour-mug", 1)]).map_err(|_| 422u16)
            }
        """,
        "src/queue/admission.rs": """
            pub const QUEUE_CAPACITY: usize = 1000;

            pub fn full_response() -> u16 {
                429
            }

            pub fn admit(depth: usize) -> Result<(), u16> {
                if depth >= QUEUE_CAPACITY { Err(full_response()) } else { Ok(()) }
            }
        """,
        "src/queue/retry_policy.rs": """
            pub const MAX_DELIVERY_ATTEMPTS: u8 = 5;

            pub fn retryable(status: u16) -> bool { matches!(status, 429 | 503) }
            pub fn may_redeliver(status: u16, attempt: u8) -> bool {
                retryable(status) && attempt < MAX_DELIVERY_ATTEMPTS
            }
        """,
        "src/queue/worker.rs": """
            use crate::{domain::order::Order, payments::{capture, provider::Provider}};
            use super::retry_policy;

            pub fn deliver(order: &mut Order, provider: &mut Provider, attempt: u8) -> Result<String, String> {
                match capture::capture_order(order, provider) {
                    Ok(reference) => Ok(reference),
                    Err(status) if retry_policy::may_redeliver(status, attempt) => Err("reschedule".into()),
                    Err(_) => Err("dead-letter".into()),
                }
            }
        """,
        "src/payments/provider.rs": """
            pub const PROVIDER_PROTOCOL_VERSION: u16 = 5;

            #[derive(Default)]
            pub struct Provider { pub unavailable: bool, pub captured_cents: u64 }

            impl Provider {
                pub fn capture(&mut self, order: &str, cents: u64) -> Result<String, u16> {
                    if self.unavailable { return Err(503); }
                    self.captured_cents += cents;
                    Ok(format!("capture:{order}"))
                }
            }
        """,
        "src/payments/authorization.rs": """
            use crate::{clients::fraud, domain::order::Order};

            pub fn authorize(order: &Order) -> Result<String, u16> {
                if fraud::requires_review(order.total_cents) { return Err(422); }
                Ok(format!("auth:{}:{}", order.tenant.id, order.id))
            }
        """,
        "src/payments/capture.rs": """
            use crate::domain::order::{Order, OrderState};
            use super::{authorization, provider::Provider};

            pub fn capture_order(order: &mut Order, provider: &mut Provider) -> Result<String, u16> {
                authorization::authorize(order)?;
                let reference = provider.capture(&order.id, order.total_cents)?;
                order.state = OrderState::Captured;
                Ok(reference)
            }
        """,
        "src/payments/refund.rs": """
            use crate::domain::order::{Order, OrderState};

            pub fn refund_reference(order: &Order, capture: &str) -> Result<String, String> {
                if order.state != OrderState::Captured || capture.is_empty() { return Err("capture required".into()); }
                Ok(format!("refund:{capture}"))
            }
        """,
        "src/settlement/window.rs": """
            pub const SETTLEMENT_WINDOW_SECONDS: u32 = 60;

            pub fn settlement_window_seconds() -> u32 { SETTLEMENT_WINDOW_SECONDS }
            pub fn batch_bucket(timestamp: u64) -> u64 { timestamp / u64::from(settlement_window_seconds()) }
        """,
        "src/settlement/ledger.rs": """
            use crate::domain::order::{Order, OrderState};

            pub fn captured_total(orders: &[Order]) -> u64 {
                orders.iter().filter(|order| order.state == OrderState::Captured)
                    .map(|order| order.total_cents).sum()
            }
        """,
        "src/settlement/reconciliation.rs": """
            use crate::{cache::{entries::Entries, settlement::refresh_summary}, domain::order::Order};
            use super::{ledger, window};

            pub fn reconcile(orders: &[Order], provider_total: u64) -> Result<u64, String> {
                let ledger_total = ledger::captured_total(orders);
                if ledger_total == provider_total { Ok(ledger_total) } else { Err("review required".into()) }
            }

            pub fn close_batch(orders: &[Order], provider_total: u64, timestamp: u64,
                               tenant: &str, entries: &mut Entries) -> Result<u64, String> {
                let total = reconcile(orders, provider_total)?;
                refresh_summary(entries, tenant, &window::batch_bucket(timestamp).to_string(), total);
                Ok(total)
            }
        """,
        "src/clients/http.rs": """
            pub const HTTP_RETRY_LIMIT: u8 = 2;

            pub fn retry_request(status: u16, attempt: u8) -> bool {
                matches!(status, 408 | 429 | 503) && attempt < HTTP_RETRY_LIMIT
            }
        """,
        "src/clients/fraud.rs": """
            pub fn requires_review(cents: u64) -> bool { cents > 500_000 }
        """,
        "src/clients/shipping.rs": """
            use crate::domain::{address, order::Order};

            pub fn shipment_reference(order: &Order, country: &str) -> Result<String, String> {
                address::validate_country(country)?;
                Ok(format!("shipment:{}:{}", order.tenant.id, order.id))
            }
        """,
        "src/observability/metrics.rs": """
            #[derive(Default)]
            pub struct Metrics { pub accepted_orders: u64, pub rejected_orders: u64 }

            impl Metrics {
                pub fn observe(&mut self, status: u16) {
                    if status < 400 { self.accepted_orders += 1; } else { self.rejected_orders += 1; }
                }
            }
        """,
        "src/observability/tracing.rs": """
            use crate::domain::{metadata, order::Order};

            pub fn order_span(order: &Order) -> String {
                format!("order/{}", metadata::correlation_id(&order.tenant.id, &order.id))
            }
        """,
        "src/config/snapshot.rs": """
            use std::collections::BTreeMap;

            pub const CONFIG_FORMAT_VERSION: u8 = 2;
            pub fn refresh(current: &BTreeMap<String, String>, updates: BTreeMap<String, String>) -> BTreeMap<String, String> {
                let mut next = current.clone();
                next.extend(updates);
                next
            }
        """,
        "src/storage/pool.rs": """
            pub const MAX_CONNECTIONS: usize = 40;
            pub fn can_acquire(in_use: usize) -> bool { in_use < MAX_CONNECTIONS }
        """,
        "src/lib.rs": """
            pub mod auth;
            pub mod cache;
            pub mod clients;
            pub mod config;
            pub mod domain;
            pub mod gateway;
            pub mod observability;
            pub mod payments;
            pub mod queue;
            pub mod settlement;
            pub mod storage;

            pub fn process_order(seed: u64) -> u64 {
                let mut store = domain::idempotency::IdempotencyStore::default();
                let mut order = gateway::ingress::submit("northwind", &seed.to_string(), 0, 1, 0, &mut store).unwrap();
                let mut provider = payments::provider::Provider::default();
                queue::worker::deliver(&mut order, &mut provider, 0).unwrap();
                let mut entries = cache::entries::Entries::default();
                entries.put(cache::keys::namespace_key(&order.tenant.id, &order.id), observability::tracing::order_span(&order));
                settlement::reconciliation::close_batch(&[order], provider.captured_cents, 3600, "northwind", &mut entries).unwrap()
            }

            #[cfg(test)]
            mod tests {
                use super::*;

                #[test]
                fn checkout_captures_and_reconciles_minor_units() { assert_eq!(process_order(1), 1944); }

                #[test]
                fn idempotency_is_scoped_to_tenant() {
                    let mut store = domain::idempotency::IdempotencyStore::default();
                    store.record("northwind", "request-1", "order-1").unwrap();
                    assert_eq!(store.get("northwind", "request-1").unwrap(), "order-1");
                    assert!(store.record("northwind", "request-1", "other-order").is_err());
                    assert!(store.get("cedar", "request-1").is_none());
                }

                #[test]
                fn independent_retry_budgets() {
                    assert!(clients::http::retry_request(503, 0));
                    assert!(!clients::http::retry_request(503, 2));
                    assert!(queue::retry_policy::may_redeliver(429, 4));
                    assert!(!queue::retry_policy::may_redeliver(503, 5));
                }

                #[test]
                fn request_renewal_returns_provider_failure() {
                    let mut state = auth::refresh::RefreshState::default();
                    state.failures_remaining = 1;
                    assert!(auth::session::renew_session(&mut state, "merchant").is_err());
                    assert_eq!(state.provider_calls, 1);
                }
            }
        """,
    }
    directories = sorted({path.split("/")[1] for path in files if path.count("/") == 2})
    for directory in directories:
        names = sorted(path.rsplit("/", 1)[1][:-3] for path in files if path.startswith(f"src/{directory}/"))
        files[f"src/{directory}/mod.rs"] = "\n".join(f"pub mod {name};" for name in names) + "\n"
    return files
