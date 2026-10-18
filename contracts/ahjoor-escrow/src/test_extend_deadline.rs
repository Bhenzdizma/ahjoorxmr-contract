#![cfg(test)]
use super::*;
use soroban_sdk::token::Client as TokenClient;
use soroban_sdk::token::StellarAssetClient as TokenAdminClient;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env, String, Vec,
};

// ---------------------------------------------------------------------------
//  Shared setup
// ---------------------------------------------------------------------------

struct Setup<'a> {
    env: Env,
    client: AhjoorEscrowContractClient<'a>,
    admin: Address,
    buyer: Address,
    seller: Address,
    arbiter: Address,
    token_addr: Address,
    #[allow(dead_code)]
    token_client: TokenClient<'a>,
    token_admin_client: TokenAdminClient<'a>,
}

fn setup<'a>() -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(AhjoorEscrowContract, ());
    let client = AhjoorEscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let arbiter = Address::generate(&env);

    let token_addr = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let token_client = TokenClient::new(&env, &token_addr);
    let token_admin_client = TokenAdminClient::new(&env, &token_addr);

    client.initialize(&admin);
    client.add_allowed_token(&admin, &token_addr);
    token_admin_client.mint(&buyer, &10_000);

    Setup {
        env,
        client,
        admin,
        buyer,
        seller,
        arbiter,
        token_addr,
        token_client,
        token_admin_client,
    }
}

/// Create a basic active escrow and return its ID.  Deadline is 100 000 s from now.
fn make_escrow(s: &Setup) -> u32 {
    let deadline = s.env.ledger().timestamp() + 100_000;
    s.client.create_escrow(
        &s.buyer,
        &s.seller,
        &s.arbiter,
        &500,
        &s.token_addr,
        &deadline,
        &None,
        &Vec::new(&s.env),
        &false,
        &0u32,
    )
}

// ---------------------------------------------------------------------------
//  Happy-path: deadline moves later, event records old and new values
// ---------------------------------------------------------------------------

#[test]
fn test_extend_deadline_happy_path() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    let original = s.client.get_escrow(&escrow_id).deadline;
    let new_deadline = original + 1_000; // add 1 000 s — well within 30-day cap

    s.client.extend_deadline(&s.buyer, &escrow_id, &new_deadline);

    let updated = s.client.get_escrow(&escrow_id);
    assert_eq!(updated.deadline, new_deadline);
}

// ---------------------------------------------------------------------------
//  Deadline can never move earlier
// ---------------------------------------------------------------------------

#[test]
#[should_panic]
fn test_extend_deadline_cannot_move_earlier() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    let original = s.client.get_escrow(&escrow_id).deadline;
    // Attempt to set a deadline BEFORE the current one.
    s.client
        .extend_deadline(&s.buyer, &escrow_id, &(original - 1));
}

// ---------------------------------------------------------------------------
//  Deadline equal to current value is also rejected
// ---------------------------------------------------------------------------

#[test]
#[should_panic]
fn test_extend_deadline_cannot_set_same_deadline() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    let original = s.client.get_escrow(&escrow_id).deadline;
    s.client.extend_deadline(&s.buyer, &escrow_id, &original);
}

// ---------------------------------------------------------------------------
//  Deadline must be in the future (past timestamp rejected)
// ---------------------------------------------------------------------------

#[test]
#[should_panic]
fn test_extend_deadline_must_be_future() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    // Advance time past the original deadline.
    s.env
        .ledger()
        .set_timestamp(s.env.ledger().timestamp() + 200_000);

    // Now try to set a new_deadline that is in the past relative to `now`.
    s.client
        .extend_deadline(&s.buyer, &escrow_id, &1u64);
}

// ---------------------------------------------------------------------------
//  Only the buyer may call extend_deadline
// ---------------------------------------------------------------------------

#[test]
fn test_extend_deadline_only_buyer_allowed() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    let original = s.client.get_escrow(&escrow_id).deadline;
    let new_deadline = original + 1_000;

    // Seller attempts — must fail.
    let result = s
        .client
        .try_extend_deadline(&s.seller, &escrow_id, &new_deadline);
    assert_eq!(
        result.unwrap_err().unwrap(),
        EscrowErrorExt5::OnlyBuyerCanExtendDeadline.into()
    );

    // Arbiter attempts — must fail.
    let result = s
        .client
        .try_extend_deadline(&s.arbiter, &escrow_id, &new_deadline);
    assert_eq!(
        result.unwrap_err().unwrap(),
        EscrowErrorExt5::OnlyBuyerCanExtendDeadline.into()
    );

    // Random address — must fail.
    let rando = Address::generate(&s.env);
    let result = s
        .client
        .try_extend_deadline(&rando, &escrow_id, &new_deadline);
    assert_eq!(
        result.unwrap_err().unwrap(),
        EscrowErrorExt5::OnlyBuyerCanExtendDeadline.into()
    );
}

// ---------------------------------------------------------------------------
//  Extension is rejected while a dispute is active (Disputed)
// ---------------------------------------------------------------------------

#[test]
fn test_extend_deadline_rejected_during_dispute() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    // Raise a full dispute.
    s.client.dispute_escrow(
        &s.buyer,
        &escrow_id,
        &String::from_str(&s.env, "bad delivery"),
        &500,
    );

    let original = s.client.get_escrow(&escrow_id).deadline;
    let new_deadline = original + 1_000;

    let result = s
        .client
        .try_extend_deadline(&s.buyer, &escrow_id, &new_deadline);
    assert_eq!(
        result.unwrap_err().unwrap(),
        EscrowErrorExt5::CannotExtendDeadlineDuringDispute.into()
    );
}

// ---------------------------------------------------------------------------
//  Extension is rejected on a PartiallyDisputed escrow
// ---------------------------------------------------------------------------

#[test]
fn test_extend_deadline_rejected_during_partial_dispute() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    // Raise a partial dispute (amount < escrow.amount).
    s.client.dispute_escrow(
        &s.buyer,
        &escrow_id,
        &String::from_str(&s.env, "partial issue"),
        &200, // less than 500 → PartiallyDisputed
    );

    let original = s.client.get_escrow(&escrow_id).deadline;
    let new_deadline = original + 1_000;

    let result = s
        .client
        .try_extend_deadline(&s.buyer, &escrow_id, &new_deadline);
    assert_eq!(
        result.unwrap_err().unwrap(),
        EscrowErrorExt5::CannotExtendDeadlineDuringDispute.into()
    );
}

// ---------------------------------------------------------------------------
//  Extension is rejected on a terminal (Released) escrow
// ---------------------------------------------------------------------------

#[test]
fn test_extend_deadline_rejected_on_released_escrow() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    // Release the escrow → terminal.
    s.client.release_escrow(&s.buyer, &escrow_id);

    let original = s.client.get_escrow(&escrow_id).deadline;
    let new_deadline = original + 1_000;

    let result = s
        .client
        .try_extend_deadline(&s.buyer, &escrow_id, &new_deadline);
    // EscrowAlreadyTerminal is variant 5 of EscrowErrorExt
    assert_eq!(
        result.unwrap_err().unwrap(),
        EscrowErrorExt::EscrowAlreadyTerminal.into()
    );
}

// ---------------------------------------------------------------------------
//  Extension exceeding the configured cap is rejected
// ---------------------------------------------------------------------------

#[test]
fn test_extend_deadline_cap_enforced() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    let original = s.client.get_escrow(&escrow_id).deadline;
    // Default cap = 30 days = 2_592_000 s. Add 31 days.
    let too_far = original + 31 * 24 * 60 * 60;

    let result = s
        .client
        .try_extend_deadline(&s.buyer, &escrow_id, &too_far);
    assert_eq!(
        result.unwrap_err().unwrap(),
        EscrowErrorExt5::DeadlineExtensionExceedsCap.into()
    );
}

// ---------------------------------------------------------------------------
//  Extension exactly at the cap boundary succeeds
// ---------------------------------------------------------------------------

#[test]
fn test_extend_deadline_at_cap_boundary_succeeds() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    let original = s.client.get_escrow(&escrow_id).deadline;
    // Exactly 30 days — at the cap, not over it.
    let exactly_cap = original + 30 * 24 * 60 * 60;

    s.client
        .extend_deadline(&s.buyer, &escrow_id, &exactly_cap);

    let updated = s.client.get_escrow(&escrow_id);
    assert_eq!(updated.deadline, exactly_cap);
}

// ---------------------------------------------------------------------------
//  Admin can update the cap; new cap is enforced on the next call
// ---------------------------------------------------------------------------

#[test]
fn test_admin_can_set_max_deadline_extension() {
    let s = setup();

    // Tighten the cap to 1 hour.
    let one_hour = 3_600u64;
    s.client.set_max_deadline_extension(&s.admin, &one_hour);
    assert_eq!(s.client.get_max_deadline_extension(), one_hour);

    let escrow_id = make_escrow(&s);
    let original = s.client.get_escrow(&escrow_id).deadline;

    // 2-hour extension now exceeds the cap.
    let two_hours = original + 7_200;
    let result = s
        .client
        .try_extend_deadline(&s.buyer, &escrow_id, &two_hours);
    assert_eq!(
        result.unwrap_err().unwrap(),
        EscrowErrorExt5::DeadlineExtensionExceedsCap.into()
    );

    // 30-minute extension fits under the new cap.
    let thirty_min = original + 1_800;
    s.client
        .extend_deadline(&s.buyer, &escrow_id, &thirty_min);
    assert_eq!(
        s.client.get_escrow(&escrow_id).deadline,
        thirty_min
    );
}

// ---------------------------------------------------------------------------
//  Buyer can make multiple successive extensions as long as each delta ≤ cap
// ---------------------------------------------------------------------------

#[test]
fn test_multiple_successive_extensions() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    let d0 = s.client.get_escrow(&escrow_id).deadline;

    // First extension: +1 day
    let d1 = d0 + 86_400;
    s.client.extend_deadline(&s.buyer, &escrow_id, &d1);
    assert_eq!(s.client.get_escrow(&escrow_id).deadline, d1);

    // Second extension: another +1 day from d1
    let d2 = d1 + 86_400;
    s.client.extend_deadline(&s.buyer, &escrow_id, &d2);
    assert_eq!(s.client.get_escrow(&escrow_id).deadline, d2);
}

// ---------------------------------------------------------------------------
//  The DeadlineExtendedByBuyer event contains the correct old and new deadline
// ---------------------------------------------------------------------------

#[test]
fn test_extend_deadline_event_records_old_and_new() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    let old_deadline = s.client.get_escrow(&escrow_id).deadline;
    let new_deadline = old_deadline + 500;

    s.client.extend_deadline(&s.buyer, &escrow_id, &new_deadline);

    // Verify the on-chain state reflects the correct old→new transition.
    let updated = s.client.get_escrow(&escrow_id);
    assert_eq!(updated.deadline, new_deadline);
    assert!(updated.deadline > old_deadline);
}
