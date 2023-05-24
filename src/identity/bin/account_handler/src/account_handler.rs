// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        account::{Account, AccountContext},
        common::AccountLifetime,
        inspect,
        interaction::Interaction,
        pre_auth::{self, produce_single_enrollment, EnrollmentState, State as PreAuthState},
        storage_lock_request,
        storage_lock_state::StorageLockState,
        wrapped_key::make_random_256_bit_generic_array,
    },
    account_common::{AccountId, AccountManagerError},
    aes_gcm::aead::generic_array::{typenum::U32, GenericArray},
    anyhow::{anyhow, format_err},
    fidl::endpoints::ServerEnd,
    fidl::prelude::*,
    fidl_fuchsia_identity_account::{AccountMarker, Error as ApiError},
    fidl_fuchsia_identity_authentication::{Enrollment, InteractionMarker, Mechanism},
    fidl_fuchsia_identity_internal::{
        AccountHandlerControlCreateAccountRequest, AccountHandlerControlRequest,
        AccountHandlerControlRequestStream,
    },
    fidl_fuchsia_process_lifecycle::{LifecycleRequest, LifecycleRequestStream},
    fuchsia_async as fasync,
    fuchsia_inspect::{Inspector, Property},
    futures::{channel::oneshot, lock::Mutex, prelude::*},
    identity_common::{EnrollmentData, PrekeyMaterial, TaskGroupError},
    std::{convert::TryInto, fmt, sync::Arc},
    storage_manager::StorageManager,
    tracing::{error, info, warn},
};

// A static enrollment id which represents the only enrollment.
const ENROLLMENT_ID: u64 = 0;

/// The states of an AccountHandler.
enum Lifecycle<SM>
where
    SM: StorageManager,
{
    /// An account has not yet been created or loaded.
    Uninitialized {
        /// A factory which produces storage managers for this account.
        storage_manager_factory: Box<dyn Fn(&AccountId) -> SM + Send + Sync>,
    },

    /// The account is initialized, and is either locked or unlocked.
    Initialized {
        lock_state: StorageLockState<SM>,
        pre_auth_state: PreAuthState,
        storage_manager: Arc<Mutex<SM>>,
    },

    /// There is no account present, and initialization is not possible.
    Finished,
}

impl<SM> fmt::Debug for Lifecycle<SM>
where
    SM: StorageManager,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Lifecycle::Uninitialized { .. } => "Uninitialized",
            Lifecycle::Initialized { lock_state: l, .. } => {
                return l.fmt(f);
            }
            Lifecycle::Finished => "Finished",
        };
        write!(f, "{name}")
    }
}

/// The core state of the AccountHandler, i.e. the Account (once it is known) and references to
/// the execution context.
pub struct AccountHandler<SM>
where
    SM: StorageManager,
{
    /// The current state of the AccountHandler state machine, optionally containing
    /// a reference to the `Account` and its pre-authentication data, depending on the
    /// state. The methods of the AccountHandler drives changes to the state.
    state: Arc<Mutex<Lifecycle<SM>>>,

    /// Lifetime for this account (ephemeral or persistent with a path).
    lifetime: AccountLifetime,

    /// Helper for outputting account handler information via fuchsia_inspect.
    inspect: Arc<inspect::AccountHandler>,

    /// The available mechanisms which can be used for Authentication.
    mechanisms: Vec<Mechanism>,
}

impl<SM> AccountHandler<SM>
where
    SM: StorageManager<Key = [u8; 32]> + Send + Sync + 'static,
{
    /// Constructs a new AccountHandler and puts it in the Uninitialized state.
    pub fn new(
        lifetime: AccountLifetime,
        inspector: &Inspector,
        mechanisms: Vec<Mechanism>,
        storage_manager_factory: Box<dyn Fn(&AccountId) -> SM + Send + Sync>,
    ) -> AccountHandler<SM> {
        let inspect = Arc::new(inspect::AccountHandler::new(inspector.root(), "uninitialized"));
        Self {
            state: Arc::new(Mutex::new(Lifecycle::Uninitialized { storage_manager_factory })),
            lifetime,
            inspect,
            mechanisms,
        }
    }

    /// Asynchronously handles the supplied stream of `AccountHandlerControlRequest` messages.
    pub async fn handle_requests_from_stream(
        &self,
        mut stream: AccountHandlerControlRequestStream,
    ) -> Result<(), anyhow::Error> {
        while let Some(req) = stream.try_next().await? {
            self.handle_request(req).await?;
        }
        Ok(())
    }

    /// Dispatches an `AccountHandlerControlRequest` message to the appropriate handler method
    /// based on its type.
    pub async fn handle_request(
        &self,
        req: AccountHandlerControlRequest,
    ) -> Result<(), fidl::Error> {
        match req {
            AccountHandlerControlRequest::CreateAccount { payload, responder } => {
                responder.send(self.create_account(payload).await.as_deref().map_err(|e| *e))?;
            }
            AccountHandlerControlRequest::Preload { pre_auth_state, responder } => {
                responder.send(self.preload(pre_auth_state).await)?;
            }
            AccountHandlerControlRequest::UnlockAccount { payload, responder } => {
                responder.send(match self.unlock_account(payload.interaction).await {
                    Ok(ref response) => Ok(response.as_deref()),
                    Err(e) => Err(e),
                })?;
            }
            AccountHandlerControlRequest::LockAccount { responder } => {
                responder.send(match self.storage_lock_account().await {
                    Ok(ref response) => Ok(response.as_deref()),
                    Err(e) => Err(e),
                })?;
            }
            AccountHandlerControlRequest::RemoveAccount { responder } => {
                responder.send(self.remove_account().await)?;
            }
            AccountHandlerControlRequest::GetAccount { account, responder } => {
                responder.send(self.get_account(account).await)?;
            }
            AccountHandlerControlRequest::Terminate { control_handle } => {
                self.terminate().await;
                control_handle.shutdown();
            }
        }
        Ok(())
    }

    /// Creates a new system account and attaches it to this handler.  Moves
    /// the handler from the `Uninitialized` to the `Initialized` state.
    async fn create_account(
        &self,
        mut payload: AccountHandlerControlCreateAccountRequest,
    ) -> Result<Vec<u8>, ApiError> {
        let account_id: AccountId = payload
            .id
            .take()
            .ok_or_else(|| {
                warn!("No account id found");
                ApiError::InvalidRequest
            })?
            .into();
        self.inspect.set_account_id(account_id);

        let mut state_lock = self.state.lock().await;
        match &*state_lock {
            Lifecycle::Uninitialized { storage_manager_factory } => {
                let disk_key: GenericArray<u8, U32> = make_random_256_bit_generic_array();

                let enrollment_state = self
                    .enroll_auth_mechanism(payload.interaction, &disk_key)
                    .await
                    .map_err(|err| {
                        warn!("Enrollment error: {:?}", err);
                        err.api_error
                    })?;
                let pre_auth_state = PreAuthState::new(account_id, enrollment_state);

                let sender = self
                    .create_storage_lock_request_sender(&pre_auth_state.enrollment_state)
                    .await
                    .map_err(|err| {
                        warn!("Error constructing lock request sender: {:?}", err);
                        err.api_error
                    })?;

                let storage_manager: Arc<Mutex<SM>> =
                    Arc::new(Mutex::new((storage_manager_factory)(&account_id)));

                let account: Account<SM> = Account::create(
                    self.lifetime.clone(),
                    Arc::clone(&storage_manager),
                    sender,
                    self.inspect.get_node(),
                )
                .await
                .map_err(|err| {
                    warn!("Error creating Account object: {:?}", err);
                    err.api_error
                })?;

                info!("CreateAccount: Successfully created new Account object");

                let () = storage_manager.lock().await.provision(disk_key.as_ref()).await.map_err(
                    |err| {
                        warn!("CreateAccount failed to provision StorageManager: {:?}", err);
                        ApiError::Resource
                    },
                )?;

                info!("CreateAccount: Successfully provisioned StorageManager instance");

                let pre_auth_state_bytes: Vec<u8> = (&pre_auth_state).try_into()?;
                *state_lock = Lifecycle::Initialized {
                    lock_state: StorageLockState::Unlocked { account: Arc::new(account) },
                    storage_manager,
                    pre_auth_state,
                };
                self.inspect.lifecycle.set("unlocked");
                info!("CreateAccount successfully completed");
                Ok(pre_auth_state_bytes)
            }
            ref invalid_state => {
                warn!("CreateAccount was called in the {:?} state", invalid_state);
                Err(ApiError::FailedPrecondition)
            }
        }
    }

    /// Loads pre-authentication state for an account.  Moves the handler from
    /// the `Uninitialized` to the `Locked` state.
    async fn preload(&self, pre_auth_state_bytes: Vec<u8>) -> Result<(), ApiError> {
        if self.lifetime == AccountLifetime::Ephemeral {
            warn!("Preload was called on an ephemeral account");
            return Err(ApiError::InvalidRequest);
        }
        let mut state_lock = self.state.lock().await;
        match &*state_lock {
            Lifecycle::Uninitialized { storage_manager_factory } => {
                let pre_auth_state = PreAuthState::try_from(pre_auth_state_bytes)?;
                self.inspect.set_account_id(*pre_auth_state.account_id());

                let storage_manager =
                    Arc::new(Mutex::new((storage_manager_factory)(pre_auth_state.account_id())));
                *state_lock = Lifecycle::Initialized {
                    lock_state: StorageLockState::Locked,
                    pre_auth_state,
                    storage_manager,
                };

                self.inspect.lifecycle.set("locked");
                Ok(())
            }
            ref invalid_state => {
                warn!("Preload was called in the {:?} state", invalid_state);
                Err(ApiError::FailedPrecondition)
            }
        }
    }

    /// Unlocks an existing system account and attaches it to this handler.
    /// If the account is enrolled with an authentication mechanism,
    /// authentication will be performed as part of this call. Moves
    /// the handler to the `Initialized` state.
    ///
    /// Optionally returns a serialized PreAuthState if it has changed.
    async fn unlock_account(
        &self,
        interaction: Option<ServerEnd<InteractionMarker>>,
    ) -> Result<Option<Vec<u8>>, ApiError> {
        let mut state_lock = self.state.lock().await;
        match &*state_lock {
            Lifecycle::Initialized { lock_state: StorageLockState::Unlocked { .. }, .. } => {
                info!(
                    "UnlockAccount was called in the Initialized::Unlocked state, quietly \
                    succeeding."
                );
                Ok(None)
            }
            Lifecycle::Initialized {
                lock_state: StorageLockState::Locked,
                pre_auth_state: pre_auth_state_ref,
                storage_manager,
            } => {
                let (prekey_material, maybe_updated_enrollment_state) = Self::authenticate(
                    &pre_auth_state_ref.enrollment_state,
                    interaction,
                    &self.mechanisms,
                )
                .await
                .map_err(|err| {
                    warn!("UnlockAccount: Authentication error: {:?}", err);
                    err.api_error
                })?;

                let sender = self
                    .create_storage_lock_request_sender(&pre_auth_state_ref.enrollment_state)
                    .await
                    .map_err(|err| {
                        warn!("UnlockAccount: Error constructing lock request sender: {:?}", err);
                        err.api_error
                    })?;
                let account: Account<SM> = Account::load(
                    self.lifetime.clone(),
                    Arc::clone(storage_manager),
                    sender,
                    self.inspect.get_node(),
                )
                .await
                .map_err(|err| err.api_error)?;

                let key: [u8; 32] =
                    Self::fetch_key(&prekey_material, &pre_auth_state_ref.enrollment_state)?;

                let () =
                    storage_manager.lock().await.unlock_storage(&key).await.map_err(|err| {
                        warn!("UnlockAccount failed to unlock StorageManager: {:?}", err);
                        ApiError::Resource
                    })?;

                let mut pre_auth_state = pre_auth_state_ref.clone();
                let pre_auth_state_bytes = maybe_updated_enrollment_state
                    .map(|updated_enrollment_state| {
                        pre_auth_state.enrollment_state = updated_enrollment_state;
                        (&pre_auth_state).try_into()
                    })
                    .transpose()?;
                *state_lock = Lifecycle::Initialized {
                    lock_state: StorageLockState::Unlocked { account: Arc::new(account) },
                    pre_auth_state,
                    storage_manager: Arc::clone(storage_manager),
                };
                self.inspect.lifecycle.set("unlocked");
                match pre_auth_state_bytes {
                    None => {
                        info!("UnlockAccount successfully completed without changing PreAuthState")
                    }
                    Some(_) => info!("UnlockAccount successfully completed with new PreAuthState"),
                }
                Ok(pre_auth_state_bytes)
            }
            ref invalid_state => {
                warn!("UnlockAccount was called in the {:?} state", invalid_state);
                Err(ApiError::FailedPrecondition)
            }
        }
    }

    /// Locks the account, terminating all open Account and Persona channels.  Moves
    /// the handler to the `Locked` state.
    ///
    /// Optionally returns a serialized PreAuthState if it has changed.
    async fn storage_lock_account(&self) -> Result<Option<Vec<u8>>, ApiError> {
        let lock_result =
            Self::storage_lock_now(Arc::clone(&self.state), Arc::clone(&self.inspect)).await;
        match &lock_result {
            Ok(None) => {
                info!("StorageLockAccount successfully completed without changing PreAuthState")
            }
            Ok(Some(_)) => info!("StorageLockAccount successfully completed with new PreAuthState"),
            Err(err) => warn!("StorageLockAccount operation failed: {:?}", err),
        }
        lock_result.map_err(|err| err.api_error)
    }

    /// Remove the active account. This method should not be retried on failure.
    async fn remove_account(&self) -> Result<(), ApiError> {
        let old_lifecycle = {
            let mut state_lock = self.state.lock().await;
            std::mem::replace(&mut *state_lock, Lifecycle::Finished)
        };

        self.inspect.lifecycle.set("finished");
        match old_lifecycle {
            Lifecycle::Initialized {
                lock_state: StorageLockState::Locked { .. },
                storage_manager,
                ..
            } => {
                storage_manager.lock().await.destroy().await.map_err(|err| {
                    warn!("RemoveAccount failed to destroy StorageManager: {:?}", err);
                    ApiError::Resource
                })?;
                // If this account was once unlocked but is now locked, it must
                // have passed through the Self::storage_lock_now(..) method, which
                // fetches the task group and cancels it.
                info!("RemoveAccount successfully deleted storage-locked account");
                Ok(())
            }
            Lifecycle::Initialized {
                lock_state: StorageLockState::Unlocked { account, .. },
                storage_manager,
                ..
            } => {
                storage_manager.lock().await.destroy().await.map_err(|err| {
                    warn!("RemoveAccount failed to destroy StorageManager: {:?}", err);
                    ApiError::Resource
                })?;
                let account_arc = account;
                // TODO(fxbug.dev/555): After this point, error recovery might
                // include putting the account back in the lock.
                if let Err(TaskGroupError::AlreadyCancelled) =
                    account_arc.task_group().cancel().await
                {
                    warn!("RemoveAccount: Task group already cancelled prior to account removal.");
                }
                // At this point we have exclusive access to the account, so we
                // move it out of the Arc to destroy it.
                let account = Arc::try_unwrap(account_arc).map_err(|_| {
                    warn!("RemoveAccount: Could not acquire exclusive access to account");
                    ApiError::Internal
                })?;
                account.remove().map_err(|(_account, err)| {
                    warn!("RemoveAccount: Could not delete account: {:?}", err);
                    err.api_error
                })?;
                info!("RemoveAccount successfully deleted storage-unlocked account");
                Ok(())
            }
            _ => {
                warn!("RemoveAccount: No account is initialized");
                Err(ApiError::FailedPrecondition)
            }
        }
    }

    /// Connects the provided `account_server_end` to the `Account` protocol
    /// served by this handler.
    async fn get_account(
        &self,
        account_server_end: ServerEnd<AccountMarker>,
    ) -> Result<(), ApiError> {
        let account_arc = match &*self.state.lock().await {
            Lifecycle::Initialized {
                lock_state: StorageLockState::Unlocked { account, .. },
                ..
            } => Arc::clone(account),
            _ => {
                warn!("GetAccount: AccountHandler is not unlocked");
                return Err(ApiError::FailedPrecondition);
            }
        };

        let context = AccountContext {};
        let stream = account_server_end.into_stream().map_err(|err| {
            warn!("GetAccount: Error opening Account channel {:?}", err);
            ApiError::Resource
        })?;

        let account_arc_clone = Arc::clone(&account_arc);
        account_arc
            .task_group()
            .spawn(|cancel| async move {
                account_arc_clone
                    .handle_requests_from_stream(&context, stream, cancel)
                    .await
                    .unwrap_or_else(|e| error!("Error handling Account channel: {:?}", e));
            })
            .await
            .map_err(|_| {
                // Since AccountHandler serves only one channel of requests in serial, this is an
                // inconsistent state rather than a conflict
                ApiError::Internal
            })
        // Potentially there will be several GetAccount calls without user interaction so don't log
        // the success case to avoid log spam.
    }

    async fn terminate(&self) {
        info!("Gracefully shutting down AccountHandler");
        let old_state = {
            let mut state_lock = self.state.lock().await;
            std::mem::replace(&mut *state_lock, Lifecycle::Finished)
        };
        if let Lifecycle::Initialized {
            lock_state: StorageLockState::Unlocked { account, .. },
            ..
        } = old_state
        {
            if account.task_group().cancel().await.is_err() {
                warn!("Task group cancelled but account is still unlocked");
            }
        }
    }

    fn fetch_key(
        prekey_material: &PrekeyMaterial,
        enrollment_state: &EnrollmentState,
    ) -> Result<[u8; 32], ApiError> {
        match enrollment_state {
            EnrollmentState::SingleEnrollment { wrapped_key_material, .. } => {
                match wrapped_key_material.unwrap_disk_key(prekey_material) {
                    Ok(key) => Ok(key),
                    Err(e) => {
                        warn!("Could not decrypt key material from SingleEnrollment: {:?}", e);
                        Err(e)
                    }
                }
            }
            EnrollmentState::NoEnrollments => {
                warn!(
                    "Could not decrypt key material from SingleEnrollment: there were no \
                      enrollments."
                );
                Err(ApiError::FailedPrecondition)
            }
        }
    }

    /// Enrolls a new authentication mechanism for the account, returning the
    /// enrollment data and prekey material from the authenticator if successful.
    async fn enroll_auth_mechanism(
        &self,
        mut interaction: Option<ServerEnd<InteractionMarker>>,
        disk_key: &GenericArray<u8, U32>,
    ) -> Result<EnrollmentState, AccountManagerError> {
        Ok(match (&self.lifetime, self.mechanisms.as_slice()) {
            (AccountLifetime::Persistent { .. }, [mechanism]) => {
                let server_end = interaction.take().ok_or_else(|| {
                    AccountManagerError::new(ApiError::InvalidRequest)
                        .with_cause(format_err!("Interaction ServerEnd missing."))
                })?;
                let (data, prekey_material) = Interaction::enroll(server_end, *mechanism).await?;

                produce_single_enrollment(*mechanism, data, prekey_material, disk_key)?
            }
            (AccountLifetime::Ephemeral, []) => pre_auth::EnrollmentState::NoEnrollments,
            (AccountLifetime::Persistent { .. }, _) => {
                // We don't support zero or multiple authentication mechanisms.
                // We already have a compile time check by providing the
                // size limit in the structured config but we include this
                // to cover zero or multiple mechanisms case.
                return Err(AccountManagerError::new(ApiError::Internal).with_cause(format_err!(
                    "CreateAccount called with unsupported number of \
                            authentication mechanisms set on a persistent account"
                )));
            }
            (AccountLifetime::Ephemeral, [_, ..]) => {
                return Err(AccountManagerError::new(ApiError::Internal).with_cause(format_err!(
                    "CreateAccount called with \
                            authentication mechanism set on an ephemeral account"
                )));
            }
        })
    }

    /// Performs an authentication attempt if appropriate. Returns pre-key
    /// material from the attempt if the account is configured with a key, and
    /// optionally a new pre-authentication state, to be written if the
    /// attempt is successful.
    async fn authenticate(
        enrollment_state: &pre_auth::EnrollmentState,
        mut interaction: Option<ServerEnd<InteractionMarker>>,
        mechanisms: &[Mechanism],
    ) -> Result<(PrekeyMaterial, Option<pre_auth::EnrollmentState>), AccountManagerError> {
        if let pre_auth::EnrollmentState::SingleEnrollment {
            ref mechanism,
            ref data,
            ref wrapped_key_material,
        } = enrollment_state
        {
            let enrollments = vec![Enrollment { id: ENROLLMENT_ID, data: data.to_vec() }];
            if !mechanisms.contains(mechanism) {
                return Err(AccountManagerError::new(ApiError::Internal).with_cause(format_err!(
                    "Enrollment mechanism {:?} is not available for authentication.",
                    mechanism
                )));
            }
            let server_end = interaction.take().ok_or_else(|| {
                AccountManagerError::new(ApiError::InvalidRequest)
                    .with_cause(format_err!("Interaction ServerEnd missing."))
            })?;
            let auth_attempt =
                Interaction::authenticate(server_end, *mechanism, enrollments).await?;

            match auth_attempt.enrollment_id {
                None => Err(AccountManagerError::new(ApiError::Internal).with_cause(format_err!(
                    "Authenticator returned an empty enrollment id during authentication."
                ))),
                Some(id) if id != ENROLLMENT_ID =>
                // TODO(dnordstrom): Error code for unexpected behavior from another component.
                {
                    Err(AccountManagerError::new(ApiError::Internal).with_cause(format_err!(
                    "Authenticator returned an unexpected enrollment id {} during authentication.",
                    id)))
                }
                _ => Ok(()),
            }?;
            // Determine whether pre-auth state should be updated
            let updated_pre_auth_state = auth_attempt.updated_enrollment_data.map(|data| {
                pre_auth::EnrollmentState::SingleEnrollment {
                    mechanism: *mechanism,
                    data: EnrollmentData(data),
                    wrapped_key_material: wrapped_key_material.clone(),
                }
            });

            let prekey_material =
                PrekeyMaterial(auth_attempt.prekey_material.ok_or_else(|| {
                    AccountManagerError::new(ApiError::Internal).with_cause(anyhow!(
                        "Authenticator unexpectedly returned no prekey material"
                    ))
                })?);

            Ok((prekey_material, updated_pre_auth_state))
        } else {
            // TODO(fxbug.dev/104199): Once we can expect enrollments in all
            // unit tests, we can remove this fake value. For now, throwing an
            // error in this case breaks several unit tests in this file which
            // do not attach enrollments.
            Ok((PrekeyMaterial(vec![]), None))
        }
    }

    /// Returns a sender which, when dispatched, causes the account handler
    /// to transition to the locked state. This method spawns
    /// a task monitoring the storage lock request (which terminates quitely if the
    /// sender is dropped). If storage lock requests are not supported for the account,
    /// depending on the pre-auth state, an unsupported storage lock request sender is
    /// returned.
    async fn create_storage_lock_request_sender(
        &self,
        enrollment_state: &pre_auth::EnrollmentState,
    ) -> Result<storage_lock_request::Sender, AccountManagerError> {
        // Storage lock requests are only supported for accounts with an enrolled
        // storage unlock mechanism
        if enrollment_state == &pre_auth::EnrollmentState::NoEnrollments {
            return Ok(storage_lock_request::Sender::NotSupported);
        }
        // Use weak pointers in order to not interfere with destruction of AccountHandler
        let state_weak = Arc::downgrade(&self.state);
        let inspect_weak = Arc::downgrade(&self.inspect);
        let (sender, receiver) = storage_lock_request::channel();
        fasync::Task::spawn(async move {
            match receiver.await {
                Ok(()) => {
                    if let (Some(state), Some(inspect)) =
                        (state_weak.upgrade(), inspect_weak.upgrade())
                    {
                        if let Err(err) = Self::storage_lock_now(state, inspect).await {
                            warn!("Storage lock request failure: {:?}", err);
                        }
                    }
                }
                Err(oneshot::Canceled) => {
                    // The sender was dropped, which is on the expected path.
                }
            }
        })
        .detach();
        Ok(sender)
    }

    /// Moves the provided lifecycle to the storage lock state, and notifies the inspect
    /// node of the change. Succeeds quitely if already storage locked.
    ///
    /// Returns a serialized PreAuthState if it's changed.
    async fn storage_lock_now(
        state: Arc<Mutex<Lifecycle<SM>>>,
        inspect: Arc<inspect::AccountHandler>,
    ) -> Result<Option<Vec<u8>>, AccountManagerError> {
        let mut state_lock = state.lock().await;
        match &mut *state_lock {
            Lifecycle::Initialized { ref mut lock_state, storage_manager, .. } => {
                match lock_state {
                    StorageLockState::Locked {} => {
                        info!(
                            "A storage lock operation was attempted in the storage locked state, \
                            quietly succeeding."
                        );
                        Ok(None)
                    }
                    StorageLockState::Unlocked { account } => {
                        let () =
                            storage_manager.lock().await.lock_storage().await.map_err(|err| {
                                warn!(
                                    "LockAccount failed to storage lock StorageManager: {:?}",
                                    err
                                );
                                AccountManagerError::new(ApiError::Internal).with_cause(err)
                            })?;

                        // Ignore AlreadyCancelled error
                        let _ = account.task_group().cancel().await;

                        *lock_state = StorageLockState::Locked;
                        inspect.lifecycle.set("locked");
                        Ok(None) // Pre-auth state remains the same so don't return it.
                    }
                }
            }
            ref invalid_state => Err(AccountManagerError::new(ApiError::FailedPrecondition)
                .with_cause(format_err!(
                    "A lock operation was attempted in the {:?} state",
                    invalid_state
                ))),
        }
    }

    /// Serially process a stream of incoming LifecycleRequest FIDL requests.
    pub async fn handle_requests_for_lifecycle(&self, mut request_stream: LifecycleRequestStream) {
        info!("Watching for lifecycle events from startup handle");
        while let Some(request) = request_stream.try_next().await.expect("read lifecycle request") {
            match request {
                LifecycleRequest::Stop { control_handle } => {
                    // `account_handler` supervises a filesystem process, which expects to
                    // receive advance notice when shutdown is imminent so that it can flush any
                    // cached writes to disk.  To uphold our end of that contract, we implement a
                    // lifecycle listener which responds to a stop request by
                    // storage locking all unlocked accounts, which in turn has
                    // the effect of gracefully stopping the filesystem
                    // and locking storage.
                    info!("Received lifecycle stop request; attempting graceful teardown");

                    match self.storage_lock_account().await {
                        Ok(_) => {
                            info!("Shutdown complete");
                        }
                        Err(e) => {
                            error!(
                                "error shutting down for lifecycle request; data may not be fully \
                                    flushed {:?}",
                                e
                            );
                        }
                    }

                    control_handle.shutdown();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;
    use fidl::endpoints::{create_endpoints, create_proxy_and_stream};
    use fidl_fuchsia_identity_internal::{
        AccountHandlerControlCreateAccountRequest, AccountHandlerControlMarker,
        AccountHandlerControlProxy, AccountHandlerControlUnlockAccountRequest,
    };
    use fuchsia_async as fasync;
    use fuchsia_async::DurationExt;
    use fuchsia_inspect::testing::AnyProperty;
    use fuchsia_inspect::{assert_data_tree, Inspector};
    use fuchsia_zircon as zx;
    use futures::future::join;
    use lazy_static::lazy_static;
    use std::sync::Arc;
    use storage_manager::minfs::StorageManager as MinfsStorageManager;
    use typed_builder::TypedBuilder;
    use unittest_util::{make_formatted_account_partition_any_key, MockDiskManager};

    lazy_static! {
        /// Assumed time between a lock request and when the account handler is locked
        static ref LOCK_REQUEST_DURATION: zx::Duration = zx::Duration::from_millis(20);

        /// A pre-authentication state with no enrollments
        static ref TEST_PRE_AUTH_EMPTY: PreAuthState =
            PreAuthState::new(
                AccountId::new(0),
                pre_auth::EnrollmentState::NoEnrollments
            );
    }

    /// An enum expressing unexpected errors that may occur during a test.
    #[derive(Debug)]
    enum AccountHandlerTestError {
        FidlError(fidl::Error),
        AccountError(ApiError),
    }

    impl From<fidl::Error> for AccountHandlerTestError {
        fn from(fidl_error: fidl::Error) -> Self {
            AccountHandlerTestError::FidlError(fidl_error)
        }
    }

    impl From<ApiError> for AccountHandlerTestError {
        fn from(err: ApiError) -> Self {
            AccountHandlerTestError::AccountError(err)
        }
    }

    type TestResult = Result<(), AccountHandlerTestError>;

    fn make_storage_manager(disk_manager: MockDiskManager) -> MinfsStorageManager<MockDiskManager> {
        MinfsStorageManager::new(disk_manager)
    }

    fn create_account_handler(
        lifetime: AccountLifetime,
        inspector: Arc<Inspector>,
        mechanisms: Vec<Mechanism>,
    ) -> (AccountHandlerControlProxy, impl Future<Output = ()>) {
        let test_object = AccountHandler::new(
            lifetime,
            &inspector,
            mechanisms,
            /*storage_manager_factory=*/
            Box::new(|_| {
                make_storage_manager(
                    MockDiskManager::new()
                        .with_partition(make_formatted_account_partition_any_key()),
                )
            }),
        );
        let (proxy, request_stream) = create_proxy_and_stream::<AccountHandlerControlMarker>()
            .expect("Failed to create proxy and stream");

        let server_fut = async move {
            test_object
                .handle_requests_from_stream(request_stream)
                .await
                .unwrap_or_else(|err| panic!("Fatal error handling test request: {err:?}"));

            // Check that no more objects are lurking in inspect
            std::mem::drop(test_object);
            assert_data_tree!(inspector, root: {});
        };

        (proxy, server_fut)
    }

    fn create_account_request(id: u64) -> AccountHandlerControlCreateAccountRequest {
        AccountHandlerControlCreateAccountRequest { id: Some(id), ..Default::default() }
    }

    #[derive(TypedBuilder)]
    struct RequestStreamTestArgs<TestFn, Fut>
    where
        TestFn: FnOnce(AccountHandlerControlProxy) -> Fut,
        Fut: Future<Output = TestResult>,
    {
        lifetime: AccountLifetime,
        inspector: Arc<Inspector>,
        mechanisms: Vec<Mechanism>,
        test_fn: TestFn,
    }

    fn request_stream_test<TestFn, Fut>(args: RequestStreamTestArgs<TestFn, Fut>)
    where
        TestFn: FnOnce(AccountHandlerControlProxy) -> Fut,
        Fut: Future<Output = TestResult>,
    {
        let mut executor = fasync::LocalExecutor::new();
        let (proxy, server_fut) =
            create_account_handler(args.lifetime, args.inspector, args.mechanisms);

        let (test_res, _server_result) =
            executor.run_singlethreaded(join((args.test_fn)(proxy), server_fut));

        assert!(test_res.is_ok());
    }

    #[test]
    fn test_get_account_before_initialization() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::default()))
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    let (_, account_server_end) = create_endpoints();
                    assert_eq!(
                        proxy.get_account(account_server_end).await?,
                        Err(ApiError::FailedPrecondition)
                    );
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_get_account_when_locked() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::default()))
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    let (_, account_server_end) = create_endpoints();
                    let pre_auth_state: Vec<u8> = (&*TEST_PRE_AUTH_EMPTY).try_into()?;
                    proxy.preload(&pre_auth_state).await??;
                    assert_eq!(
                        proxy.get_account(account_server_end).await?,
                        Err(ApiError::FailedPrecondition)
                    );
                    Ok(())
                })
                .build(),
        );
    }

    // TODO(fxb/118608): Enable this test when we have a mock Interaction object
    // which can be used to test the successful cases.
    #[ignore]
    #[test]
    fn test_double_initialize() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::default()))
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    proxy.create_account(create_account_request(TEST_ACCOUNT_ID_UINT)).await??;

                    assert_eq!(
                        proxy.create_account(create_account_request(TEST_ACCOUNT_ID_UINT)).await?,
                        Err(ApiError::FailedPrecondition)
                    );
                    Ok(())
                })
                .build(),
        );
    }

    // TODO(fxb/118608): Enable this test when we have a mock Interaction object
    // which can be used to test the successful cases.
    #[ignore]
    #[test]
    fn test_create_get_and_lock_account() {
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::default());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::clone(&inspector))
                .mechanisms(vec![Mechanism::Test])
                .test_fn(|account_handler_proxy| {
                    async move {
                        account_handler_proxy
                            .create_account(create_account_request(TEST_ACCOUNT_ID_UINT))
                            .await??;
                        assert_data_tree!(inspector, root: {
                            account_handler: contains {
                                account: contains {
                                    open_client_channels: 0u64,
                                },
                            }
                        });

                        let (account_client_end, account_server_end) = create_endpoints();
                        account_handler_proxy.get_account(account_server_end).await??;

                        assert_data_tree!(inspector, root: {
                            account_handler: contains {
                                lifecycle: "unlocked",
                                account: contains {
                                    open_client_channels: 1u64,
                                },
                            }
                        });

                        // The account channel should now be usable.
                        let account_proxy = account_client_end.into_proxy().unwrap();
                        assert_eq!(
                            account_proxy.get_auth_state().await?,
                            Err(ApiError::UnsupportedOperation)
                        );

                        // Lock the account and check that channels are closed
                        account_handler_proxy.lock_account().await??;
                        assert_data_tree!(inspector, root: {
                            account_handler: contains {
                                lifecycle: "locked",
                            }
                        });
                        assert!(account_proxy.get_auth_state().await.is_err());
                        Ok(())
                    }
                })
                .build(),
        );
    }

    // TODO(fxb/118608): Enable this test when we have a mock Interaction object
    // which can be used to test the successful cases.
    #[ignore]
    #[test]
    fn test_preload_and_unlock_existing_account() {
        // Create an account
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::default());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::clone(&inspector))
                .mechanisms(vec![Mechanism::Test])
                .test_fn(|proxy| async move {
                    proxy.create_account(create_account_request(TEST_ACCOUNT_ID_UINT)).await??;
                    assert_data_tree!(inspector, root: {
                        account_handler: contains {
                            lifecycle: "unlocked",
                        }
                    });
                    Ok(())
                })
                .build(),
        );

        // Ensure the account is persisted by unlocking it
        let inspector = Arc::new(Inspector::default());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::clone(&inspector))
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    let pre_auth_state: Vec<u8> = (&*TEST_PRE_AUTH_EMPTY).try_into()?;
                    proxy.preload(&pre_auth_state).await??;
                    assert_data_tree!(inspector, root: {
                        account_handler: contains {
                            lifecycle: "locked",
                        }
                    });
                    proxy
                        .unlock_account(AccountHandlerControlUnlockAccountRequest::default())
                        .await??;
                    assert_data_tree!(inspector, root: {
                        account_handler: contains {
                            lifecycle: "unlocked",
                        }
                    });
                    Ok(())
                })
                .build(),
        );
    }

    // TODO(fxb/118608): Enable this test when we have a mock Interaction object
    // which can be used to test the successful cases.
    #[ignore]
    #[test]
    fn test_multiple_unlocks() {
        // Create an account
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::default());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::clone(&inspector))
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    proxy.create_account(create_account_request(TEST_ACCOUNT_ID_UINT)).await??;
                    proxy.lock_account().await??;
                    proxy
                        .unlock_account(AccountHandlerControlUnlockAccountRequest::default())
                        .await??;
                    proxy.lock_account().await??;
                    proxy
                        .unlock_account(AccountHandlerControlUnlockAccountRequest::default())
                        .await??;
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_unlock_uninitialized_account() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::default()))
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    assert_eq!(
                        proxy
                            .unlock_account(AccountHandlerControlUnlockAccountRequest::default())
                            .await?,
                        Err(ApiError::FailedPrecondition)
                    );
                    Ok(())
                })
                .build(),
        );
    }

    // TODO(fxb/118608): Enable this test when we have a mock Interaction object
    // which can be used to test the successful cases.
    #[ignore]
    #[test]
    fn test_remove_initialized_account() {
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::default());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::clone(&inspector))
                .mechanisms(vec![])
                .test_fn(|proxy| {
                    async move {
                        assert_data_tree!(inspector, root: {
                            account_handler: {
                                lifecycle: "uninitialized",
                            }
                        });

                        proxy
                            .create_account(create_account_request(TEST_ACCOUNT_ID_UINT))
                            .await??;
                        assert_data_tree!(inspector, root: {
                            account_handler: {
                                account_id: TEST_ACCOUNT_ID_UINT,
                                lifecycle: "unlocked",
                                account: {
                                    open_client_channels: 0u64,
                                },
                                default_persona: {
                                    persona_id: AnyProperty,
                                    open_client_channels: 0u64,
                                },
                            }
                        });

                        // Keep an open channel to an account.
                        let (account_client_end, account_server_end) = create_endpoints();
                        proxy.get_account(account_server_end).await??;
                        let account_proxy = account_client_end.into_proxy().unwrap();

                        // Make sure remove_account() can make progress with an open channel.
                        proxy.remove_account().await??;

                        assert_data_tree!(inspector, root: {
                            account_handler: {
                                account_id: TEST_ACCOUNT_ID_UINT,
                                lifecycle: "finished",
                            }
                        });

                        // Make sure that the channel is in fact closed.
                        assert!(account_proxy.get_auth_state().await.is_err());

                        // We cannot remove twice.
                        assert_eq!(
                            proxy.remove_account().await?,
                            Err(ApiError::FailedPrecondition)
                        );
                        Ok(())
                    }
                })
                .build(),
        )
    }

    // TODO(fxb/118608): Enable this test when we have a mock Interaction object
    // which can be used to test the successful cases.
    #[ignore]
    #[test]
    fn test_remove_locked_account() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::default()))
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    proxy.create_account(create_account_request(TEST_ACCOUNT_ID_UINT)).await??;

                    // Keep an open channel to an account.
                    let (account_client_end, account_server_end) = create_endpoints();
                    proxy.get_account(account_server_end).await??;
                    let account_proxy = account_client_end.into_proxy().unwrap();

                    proxy.lock_account().await??;

                    assert_eq!(proxy.remove_account().await?, Ok(()));

                    // Make sure that the channel is in fact closed.
                    assert!(account_proxy.get_auth_state().await.is_err());

                    // We cannot remove twice.
                    assert_eq!(proxy.remove_account().await?, Err(ApiError::FailedPrecondition));

                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_remove_account_before_initialization() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::default()))
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    assert_eq!(proxy.remove_account().await?, Err(ApiError::FailedPrecondition));
                    Ok(())
                })
                .build(),
        );
    }

    // TODO(fxb/118608): Enable this test when we have a mock Interaction object
    // which can be used to test the successful cases.
    #[ignore]
    #[test]
    fn test_terminate() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::default()))
                .mechanisms(vec![])
                .test_fn(|proxy| {
                    async move {
                        proxy
                            .create_account(create_account_request(TEST_ACCOUNT_ID_UINT))
                            .await??;

                        // Keep an open channel to an account.
                        let (account_client_end, account_server_end) = create_endpoints();
                        proxy.get_account(account_server_end).await??;
                        let account_proxy = account_client_end.into_proxy().unwrap();

                        // Terminate the handler
                        proxy.terminate()?;

                        // Check that further operations fail
                        assert!(proxy.remove_account().await.is_err());
                        assert!(proxy.terminate().is_err());

                        // Make sure that the channel closed too.
                        assert!(account_proxy.get_auth_state().await.is_err());
                        Ok(())
                    }
                })
                .build(),
        );
    }

    // TODO(fxb/118608): Enable this test when we have a mock Interaction object
    // which can be used to test the successful cases.
    #[ignore]
    #[test]
    fn test_terminate_locked_account() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::default()))
                .mechanisms(vec![])
                .test_fn(|proxy| {
                    async move {
                        proxy
                            .create_account(create_account_request(TEST_ACCOUNT_ID_UINT))
                            .await??;
                        proxy.lock_account().await??;
                        proxy.terminate()?;

                        // Check that further operations fail
                        assert!(proxy
                            .unlock_account(AccountHandlerControlUnlockAccountRequest::default())
                            .await
                            .is_err());
                        assert!(proxy.terminate().is_err());
                        Ok(())
                    }
                })
                .build(),
        );
    }

    #[test]
    fn test_load_non_existing_account() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::default()))
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    let pre_auth_state: Vec<u8> = (&*TEST_PRE_AUTH_EMPTY).try_into()?;
                    // Preloading a non-existing account will succeed, for now
                    proxy.preload(&pre_auth_state).await??;
                    assert_eq!(
                        proxy
                            .unlock_account(AccountHandlerControlUnlockAccountRequest::default())
                            .await?,
                        Err(ApiError::NotFound)
                    );
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_create_account_ephemeral_with_interaction_mechanism() {
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(AccountLifetime::Ephemeral)
                .inspector(Arc::new(Inspector::default()))
                .mechanisms(vec![Mechanism::Test])
                .test_fn(|proxy| async move {
                    assert_eq!(
                        proxy
                            .create_account(AccountHandlerControlCreateAccountRequest {
                                id: Some(TEST_ACCOUNT_ID_UINT),
                                ..Default::default()
                            })
                            .await?,
                        Err(ApiError::Internal)
                    );
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_create_account_persistent_with_multiple_interaction_mechanisms() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::default()))
                .mechanisms(vec![Mechanism::Test, Mechanism::Password])
                .test_fn(|proxy| async move {
                    assert_eq!(
                        proxy
                            .create_account(AccountHandlerControlCreateAccountRequest {
                                id: Some(TEST_ACCOUNT_ID_UINT),
                                ..Default::default()
                            })
                            .await?,
                        Err(ApiError::Internal)
                    );
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_create_account_persistent_with_no_interaction_mechanisms() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::default()))
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    assert_eq!(
                        proxy
                            .create_account(AccountHandlerControlCreateAccountRequest {
                                id: Some(TEST_ACCOUNT_ID_UINT),
                                ..Default::default()
                            })
                            .await?,
                        Err(ApiError::Internal)
                    );
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_lock_request_ephemeral_account_failure() {
        let inspector = Arc::new(Inspector::default());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(AccountLifetime::Ephemeral)
                .inspector(Arc::clone(&inspector))
                .mechanisms(vec![])
                .test_fn(|account_handler_proxy| async move {
                    account_handler_proxy
                        .create_account(create_account_request(TEST_ACCOUNT_ID_UINT))
                        .await??;

                    // Get a proxy to the Account interface
                    let (account_client_end, account_server_end) = create_endpoints();
                    account_handler_proxy.get_account(account_server_end).await??;
                    let account_proxy = account_client_end.into_proxy().unwrap();

                    // Send the lock request
                    assert_eq!(
                        account_proxy.storage_lock().await?,
                        Err(ApiError::FailedPrecondition)
                    );

                    // Wait for a potentitially faulty lock request to propagate
                    fasync::Timer::new(LOCK_REQUEST_DURATION.after_now()).await;

                    // The channel is still usable
                    assert!(account_proxy.get_persona_ids().await.is_ok());

                    // The state remains initialized
                    assert_data_tree!(inspector, root: {
                        account_handler: contains {
                            lifecycle: "unlocked",
                        }
                    });
                    Ok(())
                })
                .build(),
        );
    }

    // TODO(fxb/118608): Enable this test when we have a mock Interaction object
    // which can be used to test the successful cases.
    #[ignore]
    #[test]
    fn test_lock_request_persistent_account_without_auth_mechanism() {
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::default());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::clone(&inspector))
                .mechanisms(vec![])
                .test_fn(|account_handler_proxy| async move {
                    account_handler_proxy
                        .create_account(create_account_request(TEST_ACCOUNT_ID_UINT))
                        .await??;

                    // Get a proxy to the Account interface
                    let (account_client_end, account_server_end) = create_endpoints();
                    account_handler_proxy.get_account(account_server_end).await??;
                    let account_proxy = account_client_end.into_proxy().unwrap();

                    // Send the lock request
                    assert_eq!(
                        account_proxy.storage_lock().await?,
                        Err(ApiError::FailedPrecondition)
                    );
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_create_account_without_id() {
        let inspector = Arc::new(Inspector::default());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(AccountLifetime::Ephemeral)
                .inspector(Arc::clone(&inspector))
                .mechanisms(vec![])
                .test_fn(|account_handler_proxy| async move {
                    // Send the invalid request
                    assert_eq!(
                        account_handler_proxy
                            .create_account(AccountHandlerControlCreateAccountRequest::default())
                            .await?,
                        Err(ApiError::InvalidRequest)
                    );

                    Ok(())
                })
                .build(),
        );
    }
}
