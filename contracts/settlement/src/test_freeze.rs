/// Integration tests for the global freeze circuit breaker in the settlement contract.
///
/// Covers:
///   - receive_payment blocked while frozen (SystemFrozen).
///   - batch_receive_payment blocked while frozen.
///   - withdraw_developer_balance blocked while frozen.
///   - Unfreezing restores normal behaviour.
///   - Unconfigured deployments are unaffected.
///   - set_freeze_contract is admin-only.
///   - Setting then clearing the address disables the check.
extern crate std;

use soroban_sdk::testutils::Address as _;
use soroban_sdk::{contract, contractimpl, Address, Env, Symbol, Vec};

use crate::{CalloraSettlement, CalloraSettlementClient, SettlementError, StorageKey};

// ---------------------------------------------------------------------------
// Minimal in-process freeze mock
// ---------------------------------------------------------------------------

#[contract]
pub struct MockFreeze;

#[contractimpl]
impl MockFreeze {
    pub fn is_frozen(env: Env) -> bool {
        env.storage()
            .instance()
            .get::<_, bool>(&Symbol::new(&env, "frozen"))
            .unwrap_or(false)
    }

    pub fn freeze(env: Env) {
        env.storage()
            .instance()
            .set(&Symbol::new(&env, "frozen"), &true);
    }

    pub fn unfreeze(env: Env) {
        env.storage()
            .instance()
            .set(&Symbol::new(&env, "frozen"), &false);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn create_usdc(env: &Env, admin: &Address) -> Address {
    let ca = env.register_stellar_asset_contract_v2(admin.clone());
    ca.address()
}

fn setup(env: &Env) -> (CalloraSettlementClient, Address, Address, Address, Address) {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let vault = Address::generate(env);
    let usdc_admin = Address::generate(env);
    let usdc = create_usdc(env, &usdc_admin);

    let settlement_addr = env.register(CalloraSettlement, ());
    let client = CalloraSettlementClient::new(env, &settlement_addr);
    client.init(&admin, &vault);
    client.set_usdc_token(&admin, &usdc);

    let freeze_addr = env.register(MockFreeze, ());

    (client, admin, vault, usdc, freeze_addr)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn receive_payment_blocked_when_frozen() {
    let env = Env::default();
    let (client, admin, vault, usdc, freeze_addr) = setup(&env);

    client.set_freeze_contract(&admin, &Some(freeze_addr.clone()));

    let freeze = MockFreezeClient::new(&env, &freeze_addr);
    freeze.freeze();

    let result =
        client.try_receive_payment(&vault, &1_000i128, &true, &None, &usdc, &1u32);
    assert!(
        result.is_err(),
        "receive_payment must fail while frozen"
    );
}

#[test]
fn receive_payment_succeeds_after_unfreeze() {
    let env = Env::default();
    let (client, admin, vault, usdc, freeze_addr) = setup(&env);

    client.set_freeze_contract(&admin, &Some(freeze_addr.clone()));

    let freeze = MockFreezeClient::new(&env, &freeze_addr);
    freeze.freeze();

    // Frozen: must fail
    assert!(client
        .try_receive_payment(&vault, &1_000i128, &true, &None, &usdc, &1u32)
        .is_err());

    // Unfreeze and retry
    freeze.unfreeze();
    // Should succeed
    client.receive_payment(&vault, &1_000i128, &true, &None, &usdc, &2u32);
}

#[test]
fn batch_receive_payment_blocked_when_frozen() {
    let env = Env::default();
    let (client, admin, vault, usdc, freeze_addr) = setup(&env);

    client.set_freeze_contract(&admin, &Some(freeze_addr.clone()));
    let freeze = MockFreezeClient::new(&env, &freeze_addr);
    freeze.freeze();

    let dev = Address::generate(&env);
    let items = Vec::from_array(&env, [(dev, 1_000i128)]);
    let result = client.try_batch_receive_payment(&vault, &items, &usdc, &1u32);
    assert!(result.is_err(), "batch_receive_payment must fail while frozen");
}

#[test]
fn withdraw_developer_balance_blocked_when_frozen() {
    let env = Env::default();
    let (client, admin, vault, usdc, freeze_addr) = setup(&env);

    // Credit a developer balance first (unfrozen)
    let dev = Address::generate(&env);
    client.receive_payment(&vault, &10_000i128, &false, &Some(dev.clone()), &usdc, &1u32);

    // Also fund the settlement contract with USDC
    let usdc_client = soroban_sdk::token::StellarAssetClient::new(&env, &usdc);
    let settlement_addr = env
        .storage()
        .instance()
        .get::<_, Address>(&StorageKey::Admin)
        .unwrap_or(admin.clone()); // use a placeholder
    // Fund via admin
    usdc_client.mint(&client.address, &10_000i128);

    // Now freeze
    client.set_freeze_contract(&admin, &Some(freeze_addr.clone()));
    let freeze = MockFreezeClient::new(&env, &freeze_addr);
    freeze.freeze();

    let result = client.try_withdraw_developer_balance(&dev, &5_000i128, &None);
    assert!(
        result.is_err(),
        "withdraw_developer_balance must fail while frozen"
    );
}

#[test]
fn receive_payment_unaffected_without_freeze_contract() {
    let env = Env::default();
    let (client, _admin, vault, usdc, _freeze_addr) = setup(&env);

    // No freeze contract set — receive_payment should work normally
    client.receive_payment(&vault, &1_000i128, &true, &None, &usdc, &1u32);
}

#[test]
fn set_freeze_contract_non_admin_fails() {
    let env = Env::default();
    let (client, _admin, _vault, _usdc, freeze_addr) = setup(&env);

    let stranger = Address::generate(&env);
    let result = client.try_set_freeze_contract(&stranger, &Some(freeze_addr));
    assert!(result.is_err(), "non-admin must not set freeze contract");
}

#[test]
fn set_freeze_contract_self_address_fails() {
    let env = Env::default();
    let (client, admin, _vault, _usdc, _freeze_addr) = setup(&env);

    let result = client.try_set_freeze_contract(&admin, &Some(client.address.clone()));
    assert!(result.is_err(), "settlement self-address must be rejected");
}

#[test]
fn clearing_freeze_contract_restores_normal_behaviour() {
    let env = Env::default();
    let (client, admin, vault, usdc, freeze_addr) = setup(&env);

    client.set_freeze_contract(&admin, &Some(freeze_addr.clone()));
    let freeze = MockFreezeClient::new(&env, &freeze_addr);
    freeze.freeze();

    // Frozen: must fail
    assert!(client
        .try_receive_payment(&vault, &1_000i128, &true, &None, &usdc, &1u32)
        .is_err());

    // Clear freeze contract
    client.set_freeze_contract(&admin, &None);

    // Should succeed now even though the mock freeze contract still reports frozen
    client.receive_payment(&vault, &1_000i128, &true, &None, &usdc, &2u32);
}

#[test]
fn get_freeze_contract_reflects_configuration() {
    let env = Env::default();
    let (client, admin, _vault, _usdc, freeze_addr) = setup(&env);

    assert_eq!(client.get_freeze_contract(), None);
    client.set_freeze_contract(&admin, &Some(freeze_addr.clone()));
    assert_eq!(client.get_freeze_contract(), Some(freeze_addr));
    client.set_freeze_contract(&admin, &None);
    assert_eq!(client.get_freeze_contract(), None);
}
