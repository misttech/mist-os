// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, format_err, Error},
    assert_matches::assert_matches,
    fidl::endpoints::{create_endpoints, create_proxy, ServerEnd},
    fidl_fuchsia_hardware_block_partition::Guid,
    fidl_fuchsia_identity_account::{
        AccountManagerGetAccountRequest, AccountManagerMarker,
        AccountManagerProvisionNewAccountRequest, AccountManagerProxy, AccountMarker,
        AccountMetadata, AccountProxy, AuthState, AuthStateSummary, Error as ApiError, Lifetime,
    },
    fidl_fuchsia_identity_authentication::{
        Empty, InteractionMarker, InteractionWatchStateResponse, Mechanism, Mode,
        TestAuthenticatorCondition, TestInteractionMarker, TestInteractionWatchStateResponse,
    },
    fidl_fuchsia_io as fio,
    fidl_fuchsia_logger::LogSinkMarker,
    fidl_fuchsia_sys2 as fsys2,
    fidl_fuchsia_tracing_provider::RegistryMarker,
    fuchsia_async::{DurationExt, Task, TimeoutExt},
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    fuchsia_zircon as zx,
    futures::{lock::Mutex, prelude::*},
    ramdevice_client::RamdiskClient,
    ramdisk_common::{format_zxcrypt, setup_ramdisk},
    std::ops::Deref,
    std::sync::Arc,
};

/// Type alias for the LocalAccountId FIDL type
type LocalAccountId = u64;

const TEST_AUTHENTICATOR_URL: &str = "#meta/test_authenticator.cm";

const ACCOUNT_MANAGER_URL: &str = "#meta/account_manager.cm";

const ACCOUNT_MANAGER_COMPONENT_NAME: &str = "account_manager";

/// Maximum time between a lock request and when the account is locked
const LOCK_REQUEST_DURATION: zx::Duration = zx::Duration::from_seconds(5);

// Canonically defined in //zircon/system/public/zircon/hw/gpt.h
const FUCHSIA_DATA_GUID_VALUE: [u8; 16] = [
    // 08185F0C-892D-428A-A789-DBEEC8F55E6A
    0x0c, 0x5f, 0x18, 0x08, 0x2d, 0x89, 0x8a, 0x42, 0xa7, 0x89, 0xdb, 0xee, 0xc8, 0xf5, 0x5e, 0x6a,
];
const FUCHSIA_DATA_GUID: Guid = Guid { value: FUCHSIA_DATA_GUID_VALUE };
const ACCOUNT_LABEL: &str = "account";

/// Convenience function to create an account metadata table
/// with the supplied name.
fn create_account_metadata(name: &str) -> AccountMetadata {
    AccountMetadata { name: Some(name.to_string()), ..Default::default() }
}

/// Calls provision_new_account on the supplied account_manager, returning an error on any
/// non-OK responses, or the account ID on success.
async fn provision_new_account_with_metadata(
    account_manager: &Arc<Mutex<NestedAccountManagerProxy>>,
    lifetime: Lifetime,
    metadata: Option<AccountMetadata>,
) -> Result<LocalAccountId, Error> {
    let account_manager_clone = Arc::clone(account_manager);
    let (interaction_proxy_opt, interaction_server_end_opt) = match lifetime {
        Lifetime::Persistent => {
            let (interaction_proxy, interaction_server_end) =
                create_proxy::<InteractionMarker>().expect("Failed to create interaction proxy");
            (Some(interaction_proxy), Some(interaction_server_end))
        }
        Lifetime::Ephemeral => (None, None),
    };

    // Provision a new account.
    let account_task = Task::local(async move {
        account_manager_clone
            .lock()
            .await
            .provision_new_account(AccountManagerProvisionNewAccountRequest {
                lifetime: Some(lifetime),
                interaction: interaction_server_end_opt,
                metadata,
                ..Default::default()
            })
            .await?
            .map_err(|error| format_err!("ProvisionNewAccount returned error: {:?}", error))
    });

    if let Some(interaction_proxy) = interaction_proxy_opt {
        let state = interaction_proxy.watch_state().await.expect("Failed to get interaction state");
        assert_eq!(state, InteractionWatchStateResponse::Enrollment(vec![Mechanism::Test]));

        let (_, test_interaction_server_end) = create_proxy::<TestInteractionMarker>().unwrap();
        interaction_proxy.start_test(test_interaction_server_end, Mode::Enroll).unwrap();
    }

    account_task.await
}

/// Calls provision_new_account on the supplied account_manager, returning an error on any
/// non-OK responses, or the account ID on success with a default account metadata.
async fn provision_new_account(
    account_manager: &Arc<Mutex<NestedAccountManagerProxy>>,
    lifetime: Lifetime,
) -> Result<LocalAccountId, Error> {
    provision_new_account_with_metadata(
        account_manager,
        lifetime,
        Some(create_account_metadata("test1")),
    )
    .await
}

enum InteractionOutcome {
    Succeed,
    Fail,
}

/// Convenience function to calls get_account on the supplied account_manager.
/// If `interaction_outcome` is set to InteractionOutcome::Succeed, we call
/// `SetSuccess` to indicate a successful authentication. Otherwise, we drop the
/// `Interaction` and `TestInteraction` proxies and close the client channel,
/// eventually leading to a failed authentication.
async fn get_account(
    account_manager: &Arc<Mutex<NestedAccountManagerProxy>>,
    account_id: u64,
    account_server_end: ServerEnd<AccountMarker>,
    interaction_outcome: InteractionOutcome,
) -> Result<Result<(), ApiError>, fidl::Error> {
    let account_manager_clone = Arc::clone(account_manager);
    let (interaction_proxy, interaction_server_end) =
        create_proxy::<InteractionMarker>().expect("Failed to create interaction proxy");
    let get_account_task = Task::local(async move {
        account_manager_clone
            .lock()
            .await
            .get_account(AccountManagerGetAccountRequest {
                id: Some(account_id),
                account: Some(account_server_end),
                interaction: Some(interaction_server_end),
                ..Default::default()
            })
            .await
    });

    let state = interaction_proxy.watch_state().await;
    if state.is_ok() {
        // For GetAccount operation, we expect the AccountManager to require
        // authentication using the Test Authentication mechanism.
        assert_eq!(state?, InteractionWatchStateResponse::Authenticate(vec![Mechanism::Test]));

        // Specify that we need to interact with the TestInteraction server to
        // successfully authenticate.
        let (test_interaction_proxy, test_interaction_server_end) =
            create_proxy::<TestInteractionMarker>().unwrap();
        interaction_proxy
            .start_test(test_interaction_server_end, Mode::Authenticate)
            .expect("Failed to complete the StartTest operation");

        let test_state = test_interaction_proxy
            .watch_state()
            .await
            .expect("Failed to get TestInteraction WatchState response");
        assert_eq!(
            test_state,
            TestInteractionWatchStateResponse::Waiting(vec![
                TestAuthenticatorCondition::SetSuccess(Empty)
            ])
        );

        match interaction_outcome {
            InteractionOutcome::Succeed => {
                test_interaction_proxy.set_success().expect("Failure during SetSuccess operation")
            }
            InteractionOutcome::Fail => {
                // We don't need a successful Interaction operation so we close
                // the Interaction channel to indicate an aborted attempt and
                // prevent the servers waiting indefinitely for a client response.
                std::mem::drop(interaction_proxy);
            }
        }
    }

    get_account_task.await
}

/// Utility function to stop a component using LifecycleController.
async fn stop_component(realm_ref: &RealmInstance, child_name: &str) {
    let lifecycle = realm_ref
        .root
        .connect_to_protocol_at_exposed_dir::<fsys2::LifecycleControllerMarker>()
        .expect("Failed to connect to LifecycleController");
    lifecycle
        .stop_instance(&format!("./{child_name}"))
        .await
        .expect(&format!("Failed to stop child: {child_name}"))
        .expect(&format!("Failed to unwrap stop child result: {child_name}"));
}

/// Utility function to start a component using LifecycleController.
async fn start_component(realm_ref: &RealmInstance, child_name: &str) {
    let lifecycle = realm_ref
        .root
        .connect_to_protocol_at_exposed_dir::<fsys2::LifecycleControllerMarker>()
        .expect("Failed to connect to LifecycleController");
    let (_, binder_server) = fidl::endpoints::create_endpoints();
    lifecycle
        .start_instance(&format!("./{child_name}"), binder_server)
        .await
        .expect(&format!("Failed to start child: {child_name}"))
        .expect(&format!("Failed to unwrap start child result: {child_name}"));
}

/// A proxy to an account manager running in a nested environment.
struct NestedAccountManagerProxy {
    /// Proxy to account manager.
    account_manager_proxy: AccountManagerProxy,

    /// The realm instance which the account manager is running in.
    /// Needs to be kept in scope to keep the realm alive.
    pub realm_instance: RealmInstance,

    /// The ramdisk with underlying minfs storage to provision.
    /// Needs to be kept in scope to keep the disk alive.
    _ramdisk: RamdiskClient,
}

impl Deref for NestedAccountManagerProxy {
    type Target = AccountManagerProxy;

    fn deref(&self) -> &AccountManagerProxy {
        &self.account_manager_proxy
    }
}

impl NestedAccountManagerProxy {
    /// Stop and start the account_manager component and re-initialize
    /// the nested account_manager_proxy.
    pub async fn restart(&mut self) -> Result<(), Error> {
        // TODO(jbuckland) must lock here.
        stop_component(&self.realm_instance, ACCOUNT_MANAGER_COMPONENT_NAME).await;
        start_component(&self.realm_instance, ACCOUNT_MANAGER_COMPONENT_NAME).await;

        self.account_manager_proxy = self
            .realm_instance
            .root
            .connect_to_protocol_at_exposed_dir::<AccountManagerMarker>()?;
        Ok(())
    }
}

/// Start account manager in an isolated environment and return a proxy to it.
async fn create_account_manager() -> Result<Arc<Mutex<NestedAccountManagerProxy>>, Error> {
    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_setup().await.unwrap();
    let account_manager =
        builder.add_child("account_manager", ACCOUNT_MANAGER_URL, ChildOptions::new()).await?;
    let test_authenticator = builder
        .add_child("test_authenticator", TEST_AUTHENTICATOR_URL, ChildOptions::new())
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name(
                    "fuchsia.identity.authentication.TestStorageUnlockMechanism",
                ))
                .from(&test_authenticator)
                .to(&account_manager),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LogSinkMarker>())
                .from(Ref::parent())
                .to(&account_manager),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::storage("data"))
                .from(Ref::parent())
                .to(&account_manager),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<RegistryMarker>())
                .from(Ref::parent())
                .to(&account_manager),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<AccountManagerMarker>())
                .from(&account_manager)
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fsys2::LifecycleControllerMarker>())
                .from(Ref::framework())
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.process.Launcher"))
                .from(Ref::parent())
                .to(&account_manager),
        )
        .await
        .unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::directory("dev-topological"))
                .from(Ref::child("driver_test_realm"))
                .to(&account_manager),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fsys2::LifecycleControllerMarker>())
                .from(Ref::framework())
                .to(Ref::child("driver_test_realm")),
        )
        .await
        .unwrap();

    let instance = builder.build().await?;

    let args = fidl_fuchsia_driver_test::RealmArgs {
        root_driver: Some("fuchsia-boot:///platform-bus#meta/platform-bus.cm".to_string()),
        ..Default::default()
    };
    instance.driver_test_realm_start(args).await?;

    let (ramdisk, controller) = setup_ramdisk(&instance, FUCHSIA_DATA_GUID, ACCOUNT_LABEL).await;
    format_zxcrypt(&ramdisk, ACCOUNT_LABEL, controller).await;

    let account_manager_proxy =
        instance.root.connect_to_protocol_at_exposed_dir::<AccountManagerMarker>()?;

    Ok(Arc::new(Mutex::new(NestedAccountManagerProxy {
        account_manager_proxy,
        realm_instance: instance,
        _ramdisk: ramdisk,
    })))
}

/// Locks an account and waits for the channel to close.
async fn lock_and_check(account: &AccountProxy) -> Result<(), Error> {
    account.storage_lock().await?.map_err(|err| anyhow!("Lock failed: {:?}", err))?;
    account
        .take_event_stream()
        .for_each(|_| async move {}) // Drain
        .map(|_| Ok(())) // Completed drain results in ok
        .on_timeout(LOCK_REQUEST_DURATION.after_now(), || Err(anyhow!("Locking timeout exceeded")))
        .await
}

// TODO(jsankey): Work with ComponentFramework and cramertj@ to develop a nice Rust equivalent of
// the C++ TestWithEnvironment fixture to provide isolated environments for each test case.  We
// are currently creating a new environment for account manager to run in, but the tests
// themselves run in a single environment.
#[fuchsia_async::run_singlethreaded(test)]
async fn test_provision_one_new_account() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    // Verify we initially have no accounts.
    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![]);

    let account_1 = provision_new_account(&account_manager, Lifetime::Persistent).await?;

    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![account_1]);

    Ok(())
}

// Since we only have one zxcrypt-backed partition, provisioning a second
// account will fail (since the disk is already formatted). When we switch to a
// fxfs-backed AccountManager, we can reenable this test.
#[ignore]
#[fuchsia_async::run_singlethreaded(test)]
async fn test_provision_many_new_accounts() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    // Verify we initially have no accounts.
    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![]);

    let account_1 = provision_new_account(&account_manager, Lifetime::Persistent).await?;
    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![account_1]);

    // Provision a second new account and verify it has a different ID.
    let account_2 = provision_new_account(&account_manager, Lifetime::Persistent).await?;
    assert_ne!(account_1, account_2);

    // Provision a third Persistent account
    let account_3 = provision_new_account(&account_manager, Lifetime::Persistent).await?;

    let account_ids = account_manager.lock().await.get_account_ids().await?;
    assert_eq!(account_ids.len(), 3);
    assert!(account_ids.contains(&account_1));
    assert!(account_ids.contains(&account_2));
    assert!(account_ids.contains(&account_3));

    Ok(())
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_provision_then_lock_then_unlock_account() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    let account_id = provision_new_account(&account_manager, Lifetime::Persistent).await?;

    let (account_client_end, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account_id, account_server_end, InteractionOutcome::Succeed)
            .await?,
        Ok(())
    );
    let account_proxy = account_client_end.into_proxy()?;

    // Lock the account and ensure that it's locked
    lock_and_check(&account_proxy).await?;

    // Unlock the account and re-acquire a channel
    let (account_client_end, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account_id, account_server_end, InteractionOutcome::Succeed)
            .await?,
        Ok(())
    );
    let account_proxy = account_client_end.into_proxy()?;
    assert_eq!(account_proxy.get_lifetime().await.unwrap(), Lifetime::Persistent);
    Ok(())
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_unlock_account() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    let account_id = provision_new_account(&account_manager, Lifetime::Persistent).await?;

    account_manager.lock().await.restart().await?;

    // Unlock the account and acquire a channel to it
    let (account_client_end, account_server_end) = create_endpoints();

    assert_eq!(
        get_account(&account_manager, account_id, account_server_end, InteractionOutcome::Succeed)
            .await?,
        Ok(())
    );

    let account_proxy = account_client_end.into_proxy()?;
    assert_eq!(account_proxy.get_lifetime().await.unwrap(), Lifetime::Persistent);
    Ok(())
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_provision_then_lock_then_unlock_fail_authentication() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    let account_id = provision_new_account(&account_manager, Lifetime::Persistent).await?;
    let (account_client_end, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account_id, account_server_end, InteractionOutcome::Fail)
            .await?,
        Ok(())
    );
    let account_proxy = account_client_end.into_proxy()?;

    // Lock the account and ensure that it's locked
    lock_and_check(&account_proxy).await?;

    // Attempting to unlock the account fails with a Aborted error
    // since we don't finish interaction procedure while authenticating.
    let (_, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account_id, account_server_end, InteractionOutcome::Fail)
            .await?,
        Err(ApiError::Aborted)
    );
    Ok(())
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_unlock_account_fail_authentication() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    let account_id = provision_new_account(&account_manager, Lifetime::Persistent).await?;

    // Restart the account manager, now the account should be locked
    account_manager.lock().await.restart().await?;

    // Attempting to unlock the account fails with a Aborted error
    // since we don't finish interaction procedure while authenticating.
    let (_, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account_id, account_server_end, InteractionOutcome::Fail)
            .await?,
        Err(ApiError::Aborted)
    );
    Ok(())
}

// This represents two nearly identical tests, one with ephemeral and one with persistent accounts
async fn get_account_and_persona_helper(lifetime: Lifetime) -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![]);

    let account_id = provision_new_account(&account_manager, lifetime).await?;

    // Connect a channel to the newly created account and verify it's usable.
    let (account_client_end, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account_id, account_server_end, InteractionOutcome::Succeed)
            .await?,
        Ok(())
    );
    let account_proxy = account_client_end.into_proxy()?;
    let account_auth_state = account_proxy.get_auth_state().await?;
    assert_matches!(
        account_auth_state,
        Ok(AuthState { summary: Some(AuthStateSummary::RecentlyAuthenticated), .. })
    );

    let expected_lock_result = match lifetime {
        // Cannot lock account not protected by an auth mechanism
        Lifetime::Ephemeral => Err(ApiError::FailedPrecondition),
        Lifetime::Persistent => Ok(()),
    };
    assert_eq!(account_proxy.storage_lock().await?, expected_lock_result);
    assert_eq!(account_proxy.get_lifetime().await?, lifetime);

    // Connect a channel to the account's default persona and verify it's usable.
    let (persona_client_end, persona_server_end) = create_endpoints();
    assert!(account_proxy.get_default_persona(persona_server_end).await?.is_ok());
    let persona_proxy = persona_client_end.into_proxy()?;
    let persona_auth_state = persona_proxy.get_auth_state().await?;
    assert_eq!(persona_auth_state, Err(ApiError::UnsupportedOperation));
    assert_eq!(persona_proxy.get_lifetime().await?, lifetime);

    Ok(())
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_get_persistent_account_and_persona() -> Result<(), Error> {
    get_account_and_persona_helper(Lifetime::Persistent).await
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_get_ephemeral_account_and_persona() -> Result<(), Error> {
    get_account_and_persona_helper(Lifetime::Ephemeral).await
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_one_account_deletion() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![]);

    let account_1 = provision_new_account(&account_manager, Lifetime::Persistent).await?;

    let existing_accounts = account_manager.lock().await.get_account_ids().await?;
    assert!(existing_accounts.contains(&account_1));
    assert_eq!(existing_accounts.len(), 1);

    // Delete an account and verify it is removed.
    assert_eq!(account_manager.lock().await.remove_account(account_1).await?, Ok(()));
    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![]);

    // Connecting to the deleted account should fail.
    let (_account_client_end, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account_1, account_server_end, InteractionOutcome::Succeed)
            .await?,
        Err(ApiError::NotFound)
    );
    Ok(())
}

// Since we only have one zxcrypt-backed partition, provisioning a second
// account will fail (since the disk is already formatted). When we switch to a
// fxfs-backed AccountManager, we can reenable this test.
#[ignore]
#[fuchsia_async::run_singlethreaded(test)]
async fn test_many_account_deletions() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    // Verify we initially have no accounts.
    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![]);

    let account_1 = provision_new_account(&account_manager, Lifetime::Persistent).await?;

    let account_2 = provision_new_account(&account_manager, Lifetime::Persistent).await?;
    let existing_accounts = account_manager.lock().await.get_account_ids().await?;
    assert!(existing_accounts.contains(&account_1));
    assert!(existing_accounts.contains(&account_2));
    assert_eq!(existing_accounts.len(), 2);

    // Delete an account and verify it is removed.
    assert_eq!(account_manager.lock().await.remove_account(account_1).await?, Ok(()));
    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![account_2]);

    // Connecting to the deleted account should fail.
    let (_account_client_end, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account_1, account_server_end, InteractionOutcome::Succeed)
            .await?,
        Err(ApiError::NotFound)
    );

    Ok(())
}

/// Ensure that an account manager created in a specific environment picks up the state of
/// previous instances that ran in that environment. Also check that some basic operations work on
/// accounts created in that previous lifetime.
#[fuchsia_async::run_singlethreaded(test)]
async fn test_one_lifecycle_persistent() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![]);

    let account = provision_new_account(&account_manager, Lifetime::Persistent).await?;

    let existing_accounts = account_manager.lock().await.get_account_ids().await?;
    assert_eq!(existing_accounts.len(), 1);

    // Restart account manager
    account_manager.lock().await.restart().await?;

    let existing_accounts = account_manager.lock().await.get_account_ids().await?;
    assert_eq!(existing_accounts.len(), 1); // The persistent account was kept.

    // Retrieve a persistent account that was created in the earlier lifetime
    let (_account_client_end, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account, account_server_end, InteractionOutcome::Succeed)
            .await?,
        Ok(())
    );

    // Delete an account and verify it is removed.
    assert_eq!(account_manager.lock().await.remove_account(account).await?, Ok(()));
    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![]);
    Ok(())
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_one_lifecycle_ephemeral() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![]);

    let account = provision_new_account(&account_manager, Lifetime::Ephemeral).await?;

    let existing_accounts = account_manager.lock().await.get_account_ids().await?;
    assert_eq!(existing_accounts.len(), 1);

    // Restart account manager
    account_manager.lock().await.restart().await?;

    let existing_accounts = account_manager.lock().await.get_account_ids().await?;
    assert_eq!(existing_accounts.len(), 0); // The ephemeral account was dropped

    // Make sure we can't retrieve the ephemeral account
    let (_account_client_end, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account, account_server_end, InteractionOutcome::Fail)
            .await?,
        Err(ApiError::NotFound)
    );

    Ok(())
}

// Since we only have one zxcrypt-backed partition, provisioning a second
// account will fail (since the disk is already formatted). When we switch to a
// fxfs-backed AccountManager, we can reenable this test.
#[ignore]
#[fuchsia_async::run_singlethreaded(test)]
async fn test_many_lifecycles() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    // Verify we initially have no accounts.
    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![]);

    let account_1 = provision_new_account(&account_manager, Lifetime::Persistent).await?;
    let account_2 = provision_new_account(&account_manager, Lifetime::Persistent).await?;
    let account_3 = provision_new_account(&account_manager, Lifetime::Ephemeral).await?;

    let existing_accounts = account_manager.lock().await.get_account_ids().await?;
    assert_eq!(existing_accounts.len(), 3);

    // Restart account manager
    account_manager.lock().await.restart().await?;

    let existing_accounts = account_manager.lock().await.get_account_ids().await?;
    assert_eq!(existing_accounts.len(), 2); // The ephemeral account was dropped

    // Make sure we can't retrieve the ephemeral account
    let (_account_client_end, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account_3, account_server_end, InteractionOutcome::Fail)
            .await?,
        Err(ApiError::NotFound)
    );
    // Retrieve a persistent account that was created in the earlier lifetime
    let (_account_client_end, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account_1, account_server_end, InteractionOutcome::Succeed)
            .await?,
        Ok(())
    );

    // Delete an account and verify it is removed.
    assert_eq!(account_manager.lock().await.remove_account(account_2).await?, Ok(()));
    assert_eq!(account_manager.lock().await.get_account_ids().await?, vec![account_1]);

    Ok(())
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_account_metadata_persistence() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;
    let account_metadata = create_account_metadata("test1");
    let account_1 = provision_new_account_with_metadata(
        &account_manager,
        Lifetime::Persistent,
        Some(account_metadata.clone()),
    )
    .await?;

    // Restart account manager
    account_manager.lock().await.restart().await?;

    assert_eq!(
        account_manager.lock().await.get_account_metadata(account_1).await?,
        Ok(account_metadata)
    );

    Ok(())
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_account_metadata_failures() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    // Fail if there is no metadata
    let account_manager_clone = Arc::clone(&account_manager);
    let (_, interaction_server_end) =
        create_proxy::<InteractionMarker>().expect("Failed to create interaction proxy");
    let account_without_metadata_task = Task::local(async move {
        account_manager_clone
            .lock()
            .await
            .provision_new_account(AccountManagerProvisionNewAccountRequest {
                lifetime: Some(Lifetime::Persistent),
                interaction: Some(interaction_server_end),
                metadata: None,
                ..Default::default()
            })
            .await
            .expect("Error while invoking provision_new_account")
    });
    assert_eq!(account_without_metadata_task.await, Err(ApiError::InvalidRequest));

    // Fail if metadata is invalid
    let (_, interaction_server_end) =
        create_proxy::<InteractionMarker>().expect("Failed to create interaction proxy");
    let account_with_empty_metadata_task = Task::local(async move {
        account_manager
            .lock()
            .await
            .provision_new_account(AccountManagerProvisionNewAccountRequest {
                lifetime: Some(Lifetime::Persistent),
                interaction: Some(interaction_server_end),
                metadata: Some(AccountMetadata::default()),
                ..Default::default()
            })
            .await
            .expect("Error while invoking provision_new_account")
    });
    assert_eq!(account_with_empty_metadata_task.await, Err(ApiError::InvalidRequest));

    Ok(())
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_get_data_directory() -> Result<(), Error> {
    let account_manager = create_account_manager().await?;

    let account_id = provision_new_account(&account_manager, Lifetime::Persistent).await?;

    let (account_client_end, account_server_end) = create_endpoints();
    assert_eq!(
        get_account(&account_manager, account_id, account_server_end, InteractionOutcome::Succeed)
            .await?,
        Ok(())
    );
    let account_proxy = account_client_end.into_proxy()?;

    let (_, server) = create_proxy::<fio::DirectoryMarker>().unwrap();
    assert_matches!(account_proxy.get_data_directory(server).await, Ok(Ok(())));

    Ok(())
}
