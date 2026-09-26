#![cfg(test)]
//! #938: Refund delegate management — `add_delegate` / `remove_delegate`.
//! A delegate may approve refunds for the merchant only while registered;
//! any other non-admin address is always rejected.
use super::*;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::token::StellarAssetClient as TokenAdminClient;
use soroban_sdk::{Address, Env, String};
use ahjoor_payments::{AhjoorPaymentsContract, AhjoorPaymentsContractClient};

struct Setup<'a> {
    env: Env,
    refund_client: AhjoorRefundContractClient<'a>,
    payment_client: AhjoorPaymentsContractClient<'a>,
    token_addr: Address,
    token_admin_client: TokenAdminClient<'a>,
    customer: Address,
    merchant: Address,
}

fn setup<'a>() -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();

    let payment_id = env.register(AhjoorPaymentsContract, ());
    let payment_client = AhjoorPaymentsContractClient::new(&env, &payment_id);

    let refund_id = env.register(AhjoorRefundContract, ());
    let refund_client = AhjoorRefundContractClient::new(&env, &refund_id);

    let admin = Address::generate(&env);
    let token_addr = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let token_admin_client = TokenAdminClient::new(&env, &token_addr);

    payment_client.initialize(&admin, &admin, &0u32);
    refund_client.initialize(&admin, &payment_id, &86_400u64, &None);

    let customer = Address::generate(&env);
    let merchant = Address::generate(&env);

    Setup {
        env,
        refund_client,
        payment_client,
        token_addr,
        token_admin_client,
        customer,
        merchant,
    }
}

fn request_refund(s: &Setup) -> u32 {
    s.token_admin_client.mint(&s.customer, &1_000);
    let pid = s.payment_client.create_payment(
        &s.customer,
        &s.merchant,
        &500,
        &s.token_addr,
        &None,
        &None,
        &None,
    );
    s.payment_client.complete_payment(&pid);
    s.refund_client.request_refund(
        &s.customer,
        &pid,
        &250,
        &String::from_str(&s.env, "Item not received"),
        &0u32,
    )
}

#[test]
fn test_delegate_can_approve_after_add_delegate() {
    let s = setup();
    let delegate = Address::generate(&s.env);

    s.refund_client.add_delegate(&s.merchant, &delegate);
    assert_eq!(s.refund_client.get_delegates(&s.merchant).len(), 1);

    let refund_id = request_refund(&s);
    s.refund_client.approve_refund(&delegate, &refund_id);

    let refund = s.refund_client.get_refund(&refund_id);
    assert_eq!(refund.status, RefundStatus::Approved);
    assert!(refund.approved_at.is_some());
}

#[test]
#[should_panic(expected = "Only admin or merchant delegate can approve refunds")]
fn test_removed_delegate_cannot_approve() {
    let s = setup();
    let delegate = Address::generate(&s.env);

    s.refund_client.add_delegate(&s.merchant, &delegate);
    s.refund_client.remove_delegate(&s.merchant, &delegate);
    assert_eq!(s.refund_client.get_delegates(&s.merchant).len(), 0);

    let refund_id = request_refund(&s);
    s.refund_client.approve_refund(&delegate, &refund_id);
}

#[test]
fn test_delegate_approval_only_while_registered() {
    let s = setup();
    let delegate = Address::generate(&s.env);

    s.refund_client.add_delegate(&s.merchant, &delegate);
    let first = request_refund(&s);
    s.refund_client.approve_refund(&delegate, &first);
    assert_eq!(
        s.refund_client.get_refund(&first).status,
        RefundStatus::Approved
    );

    s.refund_client.remove_delegate(&s.merchant, &delegate);
    let second = request_refund(&s);
    assert!(s
        .refund_client
        .try_approve_refund(&delegate, &second)
        .is_err());
    assert_eq!(
        s.refund_client.get_refund(&second).status,
        RefundStatus::Requested
    );
}

#[test]
#[should_panic(expected = "Only admin or merchant delegate can approve refunds")]
fn test_non_delegate_cannot_approve_even_when_merchant_has_delegates() {
    let s = setup();
    let delegate = Address::generate(&s.env);
    let stranger = Address::generate(&s.env);

    s.refund_client.add_delegate(&s.merchant, &delegate);
    let refund_id = request_refund(&s);
    s.refund_client.approve_refund(&stranger, &refund_id);
}

#[test]
#[should_panic(expected = "Only admin or merchant delegate can approve refunds")]
fn test_delegate_of_other_merchant_cannot_approve() {
    let s = setup();
    let other_merchant = Address::generate(&s.env);
    let other_delegate = Address::generate(&s.env);

    s.refund_client.add_delegate(&other_merchant, &other_delegate);
    let refund_id = request_refund(&s);
    s.refund_client.approve_refund(&other_delegate, &refund_id);
}

#[test]
#[should_panic(expected = "Address is not a delegate")]
fn test_remove_unknown_delegate_panics() {
    let s = setup();
    let never_added = Address::generate(&s.env);
    s.refund_client.remove_delegate(&s.merchant, &never_added);
}

#[test]
#[should_panic(expected = "Address is already a delegate")]
fn test_add_duplicate_delegate_panics() {
    let s = setup();
    let delegate = Address::generate(&s.env);
    s.refund_client.add_delegate(&s.merchant, &delegate);
    s.refund_client.add_delegate(&s.merchant, &delegate);
}
//! Boundary-ledger tests for #553: permissionless-after-timeout functions must
//! treat the exact-boundary ledger (now == deadline) consistently. This
//! contract's convention (matching escrow's auto_release_expired /
//! expire_cancellation / expire_seller_transfer_veto and rosca's
//! close_round / finalize_round) is an *exclusive* boundary: the window has
//! elapsed only once `now` is strictly greater than the deadline.
//!
//! ## #553 audit table
//!
//! Every permissionless-after-timeout deadline/expiry comparison named in
//! #553, and the operator each used *before* this fix. "Elapsed at
//! boundary?" answers: is `now == deadline` already considered
//! expired/elapsed? All functions are now aligned on "No" (exclusive
//! boundary — `now` must be *strictly greater than* the deadline).
//!
//! | Contract | Function | Field compared | Operator (before) | Elapsed at boundary? (before) | Elapsed at boundary? (after) |
//! | -------- | -------- | --------------- | ------------------ | ------------------------------ | ------------------------------ |
//! | escrow | `auto_release_expired` | `escrow.deadline` | `now <= deadline` blocks | No | No (unchanged) |
//! | escrow | `expire_cancellation` | `request.expires_at` | `now <= expires_at` blocks | No | No (unchanged) |
//! | escrow | `expire_seller_transfer_veto` | `proposal.veto_deadline` (ledger seq) | `seq <= veto_deadline` blocks | No | No (unchanged) |
//! | rosca | `close_round` | round deadline | `now <= deadline` blocks | No | No (unchanged) |
//! | rosca | `finalize_round` | round deadline | `now <= deadline` blocks | No | No (unchanged) |
//! | refund | `auto_approve_refund` | `requested_at + dispute_window` | `now < threshold` blocks | **Yes** | No (fixed: now `now <= threshold` blocks) |
//! | refund | `auto_cancel_expired_request` | `requested_at + cancel_window` | `now < threshold` blocks | **Yes** | No (fixed: now `now <= threshold` blocks) |
//! | refund | `auto_reject_stale_refund` | `requested_at + auto_reject_window + extension` | `now < deadline` blocks | **Yes** | No (fixed: now `now <= deadline` blocks) |
//! | refund | `settle_expired_counter_offer` | `offer.expiry` | `now <= expiry` blocks | No | No (unchanged) |
//!
//! Three of the four named refund functions treated the exact-boundary
//! ledger as already elapsed (inclusive boundary), while every escrow and
//! rosca function — and refund's own `settle_expired_counter_offer` —
//! treated it as not yet elapsed (exclusive boundary). No code comment
//! documented this as an intentional difference, so `auto_approve_refund`,
//! `auto_cancel_expired_request`, and `auto_reject_stale_refund` were
//! aligned to the exclusive-boundary convention used everywhere else.
use super::*;
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env, String};
use ahjoor_payments::{AhjoorPaymentsContract, AhjoorPaymentsContractClient};

struct Setup<'a> {
    env: Env,
    refund_client: AhjoorRefundContractClient<'a>,
    payment_client: AhjoorPaymentsContractClient<'a>,
    customer: Address,
    merchant: Address,
    token_addr: Address,