#![cfg(test)]
use super::*;
use soroban_sdk::token::Client as TokenClient;
use soroban_sdk::token::StellarAssetClient as TokenAdminClient;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env, Vec,
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

/// Create a basic active escrow and return its ID.
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

/// Seller creates a pending partial-release request. Returns the request_id.
fn make_partial_release_request(s: &Setup, escrow_id: u32, amount: i128) -> u64 {
    let jh = BytesN::from_array(&s.env, &[0u8; 32]);
    s.client
        .request_partial_release(&s.seller, &escrow_id, &amount, &jh);
    // request_id counter starts at 1.
    1u64
}

// ---------------------------------------------------------------------------
//  Default reminder window (1 h = 3 600 s)
// ---------------------------------------------------------------------------

#[test]
fn test_default_reminder_window_is_one_hour() {
    let s = setup();
    assert_eq!(s.client.get_reminder_window(), 3_600u64);
}

// ---------------------------------------------------------------------------
//  Reminder emitted inside the window
// ---------------------------------------------------------------------------

#[test]
fn test_reminder_emitted_when_inside_window() {
    let s = setup();
    let escrow_id = make_escrow(&s);
    let _request_id = make_partial_release_request(&s, escrow_id, 100);

    // Advance time so we're 30 minutes before the response_deadline.
    // response_deadline = creation_time + 86_400 (1 day default).
    // We want  now = response_deadline - 1_800  (inside the 1 h window).
    let advance = 86_400 - 1_800;
    s.env.ledger().set_timestamp(s.env.ledger().timestamp() + advance);

    let mut ids = Vec::new(&s.env);
    ids.push_back(escrow_id);
    s.client.emit_response_reminders(&ids); // must not panic

    // Reminder flag must be set.
    assert_eq!(s.client.get_reminder_sent(&escrow_id), 1u64);
}

// ---------------------------------------------------------------------------
//  No reminder emitted before the window opens
// ---------------------------------------------------------------------------

#[test]
fn test_no_reminder_before_window() {
    let s = setup();
    let escrow_id = make_escrow(&s);
    let _request_id = make_partial_release_request(&s, escrow_id, 100);

    // Only advance 1 hour — response_deadline is in 23 h, far outside window.
    s.env.ledger().set_timestamp(s.env.ledger().timestamp() + 3_600);

    let mut ids = Vec::new(&s.env);
    ids.push_back(escrow_id);
    s.client.emit_response_reminders(&ids);

    // No reminder should have been recorded.
    assert_eq!(s.client.get_reminder_sent(&escrow_id), 0u64);
}

// ---------------------------------------------------------------------------
//  Duplicate suppression — second call must not double-emit
// ---------------------------------------------------------------------------

#[test]
fn test_reminder_not_duplicated_on_second_call() {
    let s = setup();
    let escrow_id = make_escrow(&s);
    let _request_id = make_partial_release_request(&s, escrow_id, 100);

    // Enter the reminder window.
    let advance = 86_400 - 1_800;
    s.env.ledger().set_timestamp(s.env.ledger().timestamp() + advance);

    let mut ids = Vec::new(&s.env);
    ids.push_back(escrow_id);

    // First call — should set ReminderSent = 1.
    s.client.emit_response_reminders(&ids);
    assert_eq!(s.client.get_reminder_sent(&escrow_id), 1u64);

    // Second call — ReminderSent already == request_id, so nothing new emitted.
    s.client.emit_response_reminders(&ids);
    // Still 1, not changed or errored.
    assert_eq!(s.client.get_reminder_sent(&escrow_id), 1u64);
}

// ---------------------------------------------------------------------------
//  Terminal escrows are silently skipped
// ---------------------------------------------------------------------------

#[test]
fn test_terminal_escrow_skipped() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    // Release the escrow — it becomes terminal (Released).
    s.client.release_escrow(&s.buyer, &escrow_id);

    // There's no partial-release request anyway, but even if the caller
    // passes the ID the keeper must not panic.
    let mut ids = Vec::new(&s.env);
    ids.push_back(escrow_id);
    s.client.emit_response_reminders(&ids); // must not panic
    assert_eq!(s.client.get_reminder_sent(&escrow_id), 0u64);
}

// ---------------------------------------------------------------------------
//  Escrow with no pending request is silently skipped
// ---------------------------------------------------------------------------

#[test]
fn test_no_pending_request_skipped() {
    let s = setup();
    let escrow_id = make_escrow(&s);

    // Advance into what would be the reminder window but no request exists.
    s.env
        .ledger()
        .set_timestamp(s.env.ledger().timestamp() + 86_400 - 1_800);

    let mut ids = Vec::new(&s.env);
    ids.push_back(escrow_id);
    s.client.emit_response_reminders(&ids); // must not panic
    assert_eq!(s.client.get_reminder_sent(&escrow_id), 0u64);
}

// ---------------------------------------------------------------------------
//  Non-existent escrow ID is silently skipped
// ---------------------------------------------------------------------------

#[test]
fn test_nonexistent_escrow_skipped() {
    let s = setup();
    let mut ids = Vec::new(&s.env);
    ids.push_back(999u32); // never created
    s.client.emit_response_reminders(&ids); // must not panic
}

// ---------------------------------------------------------------------------
//  Batch too large → panic
// ---------------------------------------------------------------------------

#[test]
#[should_panic]
fn test_batch_too_large_panics() {
    let s = setup();
    let mut ids = Vec::new(&s.env);
    for i in 0..51u32 {
        ids.push_back(i);
    }
    s.client.emit_response_reminders(&ids);
}

// ---------------------------------------------------------------------------
//  No reminder after deadline has already passed
// ---------------------------------------------------------------------------

#[test]
fn test_no_reminder_after_deadline_passed() {
    let s = setup();
    let escrow_id = make_escrow(&s);
    let _request_id = make_partial_release_request(&s, escrow_id, 100);

    // Advance past the response_deadline entirely.
    s.env
        .ledger()
        .set_timestamp(s.env.ledger().timestamp() + 86_400 + 1);

    let mut ids = Vec::new(&s.env);
    ids.push_back(escrow_id);
    s.client.emit_response_reminders(&ids); // must not panic
    assert_eq!(s.client.get_reminder_sent(&escrow_id), 0u64);
}

// ---------------------------------------------------------------------------
//  Admin can configure the reminder window
// ---------------------------------------------------------------------------

#[test]
fn test_admin_can_set_reminder_window() {
    let s = setup();
    let new_window = 7_200u64; // 2 hours
    s.client.set_reminder_window(&s.admin, &new_window);
    assert_eq!(s.client.get_reminder_window(), new_window);
}

// ---------------------------------------------------------------------------
//  Batch with mixed valid / invalid IDs — valid ones still work
// ---------------------------------------------------------------------------

#[test]
fn test_mixed_batch_valid_and_missing() {
    let s = setup();
    let escrow_id = make_escrow(&s);
    let _request_id = make_partial_release_request(&s, escrow_id, 100);

    // Enter the reminder window.
    s.env
        .ledger()
        .set_timestamp(s.env.ledger().timestamp() + 86_400 - 1_800);

    let mut ids = Vec::new(&s.env);
    ids.push_back(999u32);    // non-existent
    ids.push_back(escrow_id); // valid
    ids.push_back(888u32);    // non-existent

    s.client.emit_response_reminders(&ids);

    // The valid escrow must have been reminded.
    assert_eq!(s.client.get_reminder_sent(&escrow_id), 1u64);
}
