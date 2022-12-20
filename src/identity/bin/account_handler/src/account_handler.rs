// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        account::{Account, AccountContext},
        common::AccountLifetime,
        inspect,
        interaction::Interaction,
        lock_request,
        pre_auth::{self, produce_single_enrollment, EnrollmentState, State as PreAuthState},
        wrapped_key::make_random_256_bit_generic_array,
    },
    account_common::{AccountId, AccountManagerError},
    aes_gcm::aead::generic_array::{typenum::U32, GenericArray},
    anyhow::{anyhow, format_err},
    fidl::endpoints::{create_endpoints, ServerEnd},
    fidl::prelude::*,
    fidl_fuchsia_identity_account::{AccountMarker, Error as ApiError},
    fidl_fuchsia_identity_authentication::{
        Enrollment, InteractionMarker, InteractionProtocolServerEnd, Mechanism,
        StorageUnlockMechanismMarker, StorageUnlockMechanismProxy,
    },
    fidl_fuchsia_identity_internal::{
        AccountHandlerControlCreateAccountRequest, AccountHandlerControlRequest,
        AccountHandlerControlRequestStream,
    },
    fidl_fuchsia_process_lifecycle::{LifecycleRequest, LifecycleRequestStream},
    fuchsia_async as fasync,
    fuchsia_component::client::connect_to_protocol_at_path,
    fuchsia_inspect::{Inspector, Property},
    futures::{channel::oneshot, lock::Mutex, prelude::*},
    identity_common::{EnrollmentData, PrekeyMaterial, TaskGroupError},
    lazy_static::lazy_static,
    std::{collections::HashMap, convert::TryInto, fmt, sync::Arc},
    storage_manager::StorageManager,
    tracing::{error, info, warn},
};

lazy_static! {
    /// Temporary pre-key material which constitutes a successful authentication
    /// attempt, for manual and automatic tests. This constant is specifically
    /// for the developer authenticator implementations
    /// (see src/identity/bin/dev_authenticator) and needs to stay in sync.
    static ref MAGIC_PREKEY: [u8; 32] = [77; 32];

    static ref DEV_AUTHENTICATION_MECHANISM_PATHS: HashMap<&'static str, &'static str> =
        HashMap::from([
            (
                "#meta/dev_authenticator_always_succeed.cm",
                "/svc/fuchsia.identity.authentication.AlwaysSucceedStorageUnlockMechanism"
            ),
            (
                "#meta/dev_authenticator_always_fail_authentication.cm",
                "/svc/fuchsia.identity.authentication.AlwaysFailStorageUnlockMechanism"
            )
        ]);
}

/// Connects to a specified authentication mechanism and return a proxy to it.
async fn get_auth_mechanism_connection(
    auth_mechanism_id: &str,
) -> Result<StorageUnlockMechanismProxy, ApiError> {
    let key = match DEV_AUTHENTICATION_MECHANISM_PATHS.get(auth_mechanism_id) {
        Some(key) => key,
        None => {
            warn!("Invalid auth mechanism id: {}", auth_mechanism_id);
            return Err(ApiError::InvalidRequest);
        }
    };
    connect_to_protocol_at_path::<StorageUnlockMechanismMarker>(key).map_err(|err| {
        warn!("Failed to connect to authenticator {:?}", err);
        ApiError::Resource
    })
}

// A static enrollment id which represents the only enrollment.
const ENROLLMENT_ID: u64 = 0;

/// The states of an AccountHandler.
enum Lifecycle<SM>
where
    SM: StorageManager,
{
    /// An account has not yet been created or loaded.
    Uninitialized,

    /// The account is locked.
    Locked { pre_auth_state: PreAuthState },

    /// The account is currently loaded and is available.
    Initialized { account: Arc<Account<SM>>, pre_auth_state: PreAuthState },

    /// There is no account present, and initialization is not possible.
    Finished,
}

impl<SM> fmt::Debug for Lifecycle<SM>
where
    SM: StorageManager,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match *self {
            Lifecycle::Uninitialized { .. } => "Uninitialized",
            Lifecycle::Locked { .. } => "Locked",
            Lifecycle::Initialized { .. } => "Initialized",
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

    // TODO(fxb/116213): Remove this once interaction is completely rolled out.
    /// Feature flag to enable/disable interaction protocol.
    is_interaction_enabled: bool,

    /// The available mechanisms which can be used for Authentication.
    mechanisms: Vec<Mechanism>,

    /// The storage manager for this account.
    storage_manager: Arc<Mutex<SM>>,
}

impl<SM> AccountHandler<SM>
where
    SM: StorageManager<Key = [u8; 32]> + Send + Sync + 'static,
{
    /// Constructs a new AccountHandler and puts it in the Uninitialized state.
    pub fn new(
        lifetime: AccountLifetime,
        inspector: &Inspector,
        is_interaction_enabled: bool,
        mechanisms: Vec<Mechanism>,
        storage_manager: SM,
    ) -> AccountHandler<SM> {
        let inspect = Arc::new(inspect::AccountHandler::new(inspector.root(), "uninitialized"));
        Self {
            state: Arc::new(Mutex::new(Lifecycle::Uninitialized)),
            lifetime,
            inspect,
            is_interaction_enabled,
            mechanisms,
            storage_manager: Arc::new(Mutex::new(storage_manager)),
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
                let mut response = self.create_account(payload).await;
                responder.send(&mut response)?;
            }
            AccountHandlerControlRequest::Preload { pre_auth_state, responder } => {
                let mut response = self.preload(pre_auth_state).await;
                responder.send(&mut response)?;
            }
            AccountHandlerControlRequest::UnlockAccount { payload, responder } => {
                let mut response = self.unlock_account(payload.interaction).await;
                responder.send(&mut response)?;
            }
            AccountHandlerControlRequest::LockAccount { responder } => {
                let mut response = self.lock_account().await;
                responder.send(&mut response)?;
            }
            AccountHandlerControlRequest::RemoveAccount { responder } => {
                let mut response = self.remove_account().await;
                responder.send(&mut response)?;
            }
            AccountHandlerControlRequest::GetAccount { account, responder } => {
                let mut response = self.get_account(account).await;
                responder.send(&mut response)?;
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
    // TODO(fxb/104199): Remove auth_mechanism once interaction is used for tests.
    async fn create_account(
        &self,
        mut payload: AccountHandlerControlCreateAccountRequest,
    ) -> Result<Vec<u8>, ApiError> {
        let maybe_auth_mechanism_id = payload.auth_mechanism_id;
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
            Lifecycle::Uninitialized => {
                let disk_key: GenericArray<u8, U32> = make_random_256_bit_generic_array();

                let enrollment_state = self
                    .enroll_auth_mechanism(maybe_auth_mechanism_id, payload.interaction, &disk_key)
                    .await
                    .map_err(|err| {
                        warn!("Enrollment error: {:?}", err);
                        err.api_error
                    })?;
                let pre_auth_state = PreAuthState::new(account_id, enrollment_state);

                let sender = self
                    .create_lock_request_sender(&pre_auth_state.enrollment_state)
                    .await
                    .map_err(|err| {
                        warn!("Error constructing lock request sender: {:?}", err);
                        err.api_error
                    })?;
                let account: Account<SM> = Account::create(
                    self.lifetime.clone(),
                    Arc::clone(&self.storage_manager),
                    sender,
                    self.inspect.get_node(),
                )
                .await
                .map_err(|err| err.api_error)?;

                let () =
                    self.storage_manager.lock().await.provision(disk_key.as_ref()).await.map_err(
                        |err| {
                            warn!("CreateAccount failed to provision StorageManager: {:?}", err);
                            ApiError::Resource
                        },
                    )?;

                let pre_auth_state_bytes: Vec<u8> = (&pre_auth_state).try_into()?;
                *state_lock = Lifecycle::Initialized { account: Arc::new(account), pre_auth_state };
                self.inspect.lifecycle.set("initialized");
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
            Lifecycle::Uninitialized => {
                let pre_auth_state = PreAuthState::try_from(pre_auth_state_bytes)?;
                self.inspect.set_account_id(*pre_auth_state.account_id());

                *state_lock = Lifecycle::Locked { pre_auth_state };

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
            Lifecycle::Initialized { .. } => {
                info!("UnlockAccount was called in the Initialized state, quietly succeeding.");
                Ok(None)
            }
            Lifecycle::Locked { pre_auth_state: pre_auth_state_ref } => {
                let (prekey_material, maybe_updated_enrollment_state) = Self::authenticate(
                    self.is_interaction_enabled,
                    &pre_auth_state_ref.enrollment_state,
                    interaction,
                    &self.mechanisms,
                )
                .await
                .map_err(|err| {
                    warn!("Authentication error: {:?}", err);
                    err.api_error
                })?;

                let sender = self
                    .create_lock_request_sender(&pre_auth_state_ref.enrollment_state)
                    .await
                    .map_err(|err| {
                        warn!("Error constructing lock request sender: {:?}", err);
                        err.api_error
                    })?;
                let account: Account<SM> = Account::load(
                    self.lifetime.clone(),
                    Arc::clone(&self.storage_manager),
                    sender,
                    self.inspect.get_node(),
                )
                .await
                .map_err(|err| err.api_error)?;

                let key: [u8; 32] =
                    Self::fetch_key(&prekey_material, &pre_auth_state_ref.enrollment_state)?;

                let () = self.storage_manager.lock().await.unlock_storage(&key).await.map_err(
                    |err| {
                        warn!("UnlockAccount failed to unlock StorageManager: {:?}", err);
                        ApiError::Resource
                    },
                )?;

                let mut pre_auth_state = pre_auth_state_ref.clone();
                let pre_auth_state_bytes = maybe_updated_enrollment_state
                    .map(|updated_enrollment_state| {
                        pre_auth_state.enrollment_state = updated_enrollment_state;
                        (&pre_auth_state).try_into()
                    })
                    .transpose()?;
                *state_lock = Lifecycle::Initialized { account: Arc::new(account), pre_auth_state };
                self.inspect.lifecycle.set("initialized");
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
    async fn lock_account(&self) -> Result<Option<Vec<u8>>, ApiError> {
        Self::lock_now(
            Arc::clone(&self.state),
            Arc::clone(&self.storage_manager),
            Arc::clone(&self.inspect),
        )
        .await
        .map_err(|err| {
            warn!("LockAccount call failed: {:?}", err);
            err.api_error
        })
    }

    /// Remove the active account. This method should not be retried on failure.
    async fn remove_account(&self) -> Result<(), ApiError> {
        let old_lifecycle = {
            let mut state_lock = self.state.lock().await;
            std::mem::replace(&mut *state_lock, Lifecycle::Finished)
        };

        self.inspect.lifecycle.set("finished");
        match old_lifecycle {
            Lifecycle::Locked { .. } => {
                self.storage_manager.lock().await.destroy().await.map_err(|err| {
                    warn!("remove_account failed to destroy StorageManager: {:?}", err);
                    ApiError::Resource
                })?;
                // If this account was once unlocked but is now locked, it must
                // have passed through the Self::lock_now(..) method, which
                // fetches the task group and cancels it.
                info!("Deleted account");
                Ok(())
            }
            Lifecycle::Initialized { account, .. } => {
                self.storage_manager.lock().await.destroy().await.map_err(|err| {
                    warn!("remove_account failed to destroy StorageManager: {:?}", err);
                    ApiError::Resource
                })?;
                let account_arc = account;
                // TODO(fxbug.dev/555): After this point, error recovery might
                // include putting the account back in the lock.
                if let Err(TaskGroupError::AlreadyCancelled) =
                    account_arc.task_group().cancel().await
                {
                    warn!("Task group was already cancelled prior to account removal.");
                }
                // At this point we have exclusive access to the account, so we
                // move it out of the Arc to destroy it.
                let account = Arc::try_unwrap(account_arc).map_err(|_| {
                    warn!("Could not acquire exclusive access to account");
                    ApiError::Internal
                })?;
                account.remove().map_err(|(_account, err)| {
                    warn!("Could not delete account: {:?}", err);
                    err.api_error
                })?;
                info!("Deleted account");
                Ok(())
            }
            _ => {
                warn!("No account is initialized");
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
            Lifecycle::Initialized { account, .. } => Arc::clone(account),
            _ => {
                warn!("AccountHandler is not initialized");
                return Err(ApiError::FailedPrecondition);
            }
        };

        let context = AccountContext {};
        let stream = account_server_end.into_stream().map_err(|err| {
            warn!("Error opening Account channel {:?}", err);
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
    }

    async fn terminate(&self) {
        info!("Gracefully shutting down AccountHandler");
        let old_state = {
            let mut state_lock = self.state.lock().await;
            std::mem::replace(&mut *state_lock, Lifecycle::Finished)
        };
        if let Lifecycle::Initialized { account, .. } = old_state {
            if account.task_group().cancel().await.is_err() {
                warn!("Task group cancelled but account is still initialized");
            }
        }
    }

    /// Enrolls a new authentication mechanism for the account, returning the
    /// enrollment data and prekey material from the authenticator if successful.
    async fn enroll_auth_mechanism(
        &self,
        maybe_auth_mechanism_id: Option<String>,
        interaction: Option<ServerEnd<InteractionMarker>>,
        disk_key: &GenericArray<u8, U32>,
    ) -> Result<EnrollmentState, AccountManagerError> {
        let enrollment_state = if !self.is_interaction_enabled {
            self.enroll_auth_mechanism_without_interaction(maybe_auth_mechanism_id, disk_key)
                .await?
        } else {
            // If interaction mechanisms are enabled, use the interaction
            // protocol for authenticator challenges.
            self.enroll_auth_mechanism_with_interaction(interaction, disk_key).await?
        };

        Ok(enrollment_state)
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
                // TODO(fxbug.dev/104199): Once we can expect enrollments in all
                // unit tests, we can remove this fake value.
                Ok(*MAGIC_PREKEY)
            }
        }
    }

    async fn enroll_auth_mechanism_without_interaction(
        &self,
        maybe_auth_mechanism_id: Option<String>,
        disk_key: &GenericArray<u8, U32>,
    ) -> Result<EnrollmentState, AccountManagerError> {
        Ok(match (&self.lifetime, maybe_auth_mechanism_id) {
            (AccountLifetime::Persistent { .. }, Some(auth_mechanism_id)) => {
                let (_, test_interaction_server_end) = create_endpoints().unwrap();
                let mut test_ipse = InteractionProtocolServerEnd::Test(test_interaction_server_end);
                let auth_mechanism_proxy =
                    get_auth_mechanism_connection(&auth_mechanism_id).await?;
                let (data, prekey_material) = auth_mechanism_proxy
                    .enroll(&mut test_ipse)
                    .await
                    .map_err(|err| {
                        AccountManagerError::new(ApiError::Unknown)
                            .with_cause(format_err!("Error connecting to authenticator: {:?}", err))
                    })?
                    .map_err(|authenticator_err| {
                        AccountManagerError::new(ApiError::Unknown).with_cause(format_err!(
                            "Error while enrolling account: {:?}",
                            authenticator_err
                        ))
                    })
                    .map(|(enrollment_data, prekey_material)| {
                        (EnrollmentData(enrollment_data), PrekeyMaterial(prekey_material))
                    })?;
                // TODO(fxb/116213): Remove the clone once we don't need to
                // return the prekey for checking outside of this block.
                produce_single_enrollment(
                    auth_mechanism_id,
                    Mechanism::Test,
                    data,
                    prekey_material,
                    disk_key,
                )?
            }
            (AccountLifetime::Ephemeral, Some(_)) => {
                return Err(AccountManagerError::new(ApiError::InvalidRequest).with_cause(
                    format_err!(
                        "CreateAccount called with \
                            auth_mechanism_id set on an ephemeral account"
                    ),
                ));
            }
            (_, None) => pre_auth::EnrollmentState::NoEnrollments,
        })
    }

    async fn enroll_auth_mechanism_with_interaction(
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
                // TODO(fxb/116213): Remove the clone once we don't need to
                // return the prekey for checking outside of this block.
                produce_single_enrollment(
                    "".to_string(),
                    *mechanism,
                    data,
                    prekey_material,
                    disk_key,
                )?
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
        is_interaction_enabled: bool,
        enrollment_state: &pre_auth::EnrollmentState,
        mut interaction: Option<ServerEnd<InteractionMarker>>,
        mechanisms: &[Mechanism],
    ) -> Result<(PrekeyMaterial, Option<pre_auth::EnrollmentState>), AccountManagerError> {
        if let pre_auth::EnrollmentState::SingleEnrollment {
            ref auth_mechanism_id,
            ref mechanism,
            ref data,
            ref wrapped_key_material,
        } = enrollment_state
        {
            let mut enrollments = vec![Enrollment { id: ENROLLMENT_ID, data: data.to_vec() }];
            let auth_attempt = if !is_interaction_enabled {
                let (_, test_interaction_server_end) = create_endpoints().unwrap();
                let mut test_ipse = InteractionProtocolServerEnd::Test(test_interaction_server_end);
                let auth_mechanism_proxy = get_auth_mechanism_connection(auth_mechanism_id).await?;
                let fut =
                    auth_mechanism_proxy.authenticate(&mut test_ipse, &mut enrollments.iter_mut());
                fut.await.map_err(|err| {
                    AccountManagerError::new(ApiError::Unknown)
                        .with_cause(format_err!("Error connecting to authenticator: {:?}", err))
                })??
            } else {
                if !mechanisms.contains(mechanism) {
                    return Err(AccountManagerError::new(ApiError::Internal).with_cause(
                        format_err!(
                            "Enrollment mechanism {:?} is not available for authentication.",
                            mechanism
                        ),
                    ));
                }
                let server_end = interaction.take().ok_or_else(|| {
                    AccountManagerError::new(ApiError::InvalidRequest)
                        .with_cause(format_err!("Interaction ServerEnd missing."))
                })?;
                Interaction::authenticate(server_end, *mechanism, enrollments).await?
            };
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
                    auth_mechanism_id: auth_mechanism_id.to_string(),
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
    /// a task monitoring the lock request (which terminates quitely if the
    /// sender is dropped). If lock requests are not supported for the account,
    /// depending on the pre-auth state, an unsupported lock request sender is
    /// returned.
    async fn create_lock_request_sender(
        &self,
        enrollment_state: &pre_auth::EnrollmentState,
    ) -> Result<lock_request::Sender, AccountManagerError> {
        // Lock requests are only supported for accounts with an enrolled
        // storage unlock mechanism
        if enrollment_state == &pre_auth::EnrollmentState::NoEnrollments {
            return Ok(lock_request::Sender::NotSupported);
        }
        // Use weak pointers in order to not interfere with destruction of AccountHandler
        let state_weak = Arc::downgrade(&self.state);
        let storage_manager_weak = Arc::downgrade(&self.storage_manager);
        let inspect_weak = Arc::downgrade(&self.inspect);
        let (sender, receiver) = lock_request::channel();
        fasync::Task::spawn(async move {
            match receiver.await {
                Ok(()) => {
                    if let (Some(state), Some(storage_manager), Some(inspect)) = (
                        state_weak.upgrade(),
                        storage_manager_weak.upgrade(),
                        inspect_weak.upgrade(),
                    ) {
                        if let Err(err) = Self::lock_now(state, storage_manager, inspect).await {
                            warn!("Lock request failure: {:?}", err);
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

    /// Moves the provided lifecycle to the lock state, and notifies the inspect
    /// node of the change. Succeeds quitely if already locked.
    ///
    /// Returns a serialized PreAuthState if it's changed.
    async fn lock_now(
        state: Arc<Mutex<Lifecycle<SM>>>,
        storage_manager: Arc<Mutex<SM>>,
        inspect: Arc<inspect::AccountHandler>,
    ) -> Result<Option<Vec<u8>>, AccountManagerError> {
        let mut state_lock = state.lock().await;
        match &*state_lock {
            Lifecycle::Locked { .. } => {
                info!("A lock operation was attempted in the locked state, quietly succeeding.");
                Ok(None)
            }
            Lifecycle::Initialized { account, pre_auth_state } => {
                let () = storage_manager.lock().await.lock_storage().await.map_err(|err| {
                    warn!("LockAccount failed to lock StorageManager: {:?}", err);
                    AccountManagerError::new(ApiError::Internal).with_cause(err)
                })?;
                let _ = account.task_group().cancel().await; // Ignore AlreadyCancelled error

                // TODO(apsbhatia): Explore better alternatives to avoid cloning here.
                let new_state = Lifecycle::Locked { pre_auth_state: pre_auth_state.clone() };
                *state_lock = new_state;
                inspect.lifecycle.set("locked");
                Ok(None) // Pre-auth state remains the same so don't return it.
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
                    // lifecycle listener which responds to a stop request by locking all unlocked
                    // accounts, which in turn has the effect of gracefully stopping the filesystem
                    // and locking storage.
                    info!("Received lifecycle stop request; attempting graceful teardown");

                    match self.lock_account().await {
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
    use identity_testutil::{make_formatted_account_partition_any_key, MockDiskManager};
    use lazy_static::lazy_static;
    use std::sync::Arc;
    use storage_manager::minfs::StorageManager as MinfsStorageManager;
    use typed_builder::TypedBuilder;

    const TEST_AUTH_MECHANISM_ID: &str = "<AUTH MECHANISM ID>";

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
        is_interaction_enabled: bool,
        mechanisms: Vec<Mechanism>,
    ) -> (AccountHandlerControlProxy, impl Future<Output = ()>) {
        let test_object = AccountHandler::new(
            lifetime,
            &inspector,
            is_interaction_enabled,
            mechanisms,
            /*storage_manager=*/
            make_storage_manager(
                MockDiskManager::new().with_partition(make_formatted_account_partition_any_key()),
            ),
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
        AccountHandlerControlCreateAccountRequest {
            id: Some(id),
            ..AccountHandlerControlCreateAccountRequest::EMPTY
        }
    }

    #[derive(TypedBuilder)]
    struct RequestStreamTestArgs<TestFn, Fut>
    where
        TestFn: FnOnce(AccountHandlerControlProxy) -> Fut,
        Fut: Future<Output = TestResult>,
    {
        lifetime: AccountLifetime,
        inspector: Arc<Inspector>,
        is_interaction_enabled: bool,
        mechanisms: Vec<Mechanism>,
        test_fn: TestFn,
    }

    fn request_stream_test<TestFn, Fut>(args: RequestStreamTestArgs<TestFn, Fut>)
    where
        TestFn: FnOnce(AccountHandlerControlProxy) -> Fut,
        Fut: Future<Output = TestResult>,
    {
        let mut executor = fasync::LocalExecutor::new().expect("Failed to create executor");
        let (proxy, server_fut) = create_account_handler(
            args.lifetime,
            args.inspector,
            args.is_interaction_enabled,
            args.mechanisms,
        );

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
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    let (_, account_server_end) = create_endpoints().unwrap();
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
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    let (_, account_server_end) = create_endpoints().unwrap();
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

    #[test]
    fn test_double_initialize() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(false)
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

    #[test]
    fn test_create_get_and_lock_account() {
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::clone(&inspector))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
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

                        let (account_client_end, account_server_end) = create_endpoints().unwrap();
                        account_handler_proxy.get_account(account_server_end).await??;

                        assert_data_tree!(inspector, root: {
                            account_handler: contains {
                                lifecycle: "initialized",
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

    #[test]
    fn test_preload_and_unlock_existing_account() {
        // Create an account
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::clone(&inspector))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    proxy.create_account(create_account_request(TEST_ACCOUNT_ID_UINT)).await??;
                    assert_data_tree!(inspector, root: {
                        account_handler: contains {
                            lifecycle: "initialized",
                        }
                    });
                    Ok(())
                })
                .build(),
        );

        // Ensure the account is persisted by unlocking it
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::clone(&inspector))
                .is_interaction_enabled(false)
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
                        .unlock_account(AccountHandlerControlUnlockAccountRequest::EMPTY)
                        .await??;
                    assert_data_tree!(inspector, root: {
                        account_handler: contains {
                            lifecycle: "initialized",
                        }
                    });
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_multiple_unlocks() {
        // Create an account
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::clone(&inspector))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    proxy.create_account(create_account_request(TEST_ACCOUNT_ID_UINT)).await??;
                    proxy.lock_account().await??;
                    proxy
                        .unlock_account(AccountHandlerControlUnlockAccountRequest::EMPTY)
                        .await??;
                    proxy.lock_account().await??;
                    proxy
                        .unlock_account(AccountHandlerControlUnlockAccountRequest::EMPTY)
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
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    assert_eq!(
                        proxy
                            .unlock_account(AccountHandlerControlUnlockAccountRequest::EMPTY)
                            .await?,
                        Err(ApiError::FailedPrecondition)
                    );
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_remove_initialized_account() {
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::clone(&inspector))
                .is_interaction_enabled(false)
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
                                lifecycle: "initialized",
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
                        let (account_client_end, account_server_end) = create_endpoints().unwrap();
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

    #[test]
    fn test_remove_locked_account() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    proxy.create_account(create_account_request(TEST_ACCOUNT_ID_UINT)).await??;

                    // Keep an open channel to an account.
                    let (account_client_end, account_server_end) = create_endpoints().unwrap();
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
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    assert_eq!(proxy.remove_account().await?, Err(ApiError::FailedPrecondition));
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_terminate() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|proxy| {
                    async move {
                        proxy
                            .create_account(create_account_request(TEST_ACCOUNT_ID_UINT))
                            .await??;

                        // Keep an open channel to an account.
                        let (account_client_end, account_server_end) = create_endpoints().unwrap();
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

    #[test]
    fn test_terminate_locked_account() {
        let location = TempLocation::new();
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(false)
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
                            .unlock_account(AccountHandlerControlUnlockAccountRequest::EMPTY)
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
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    let pre_auth_state: Vec<u8> = (&*TEST_PRE_AUTH_EMPTY).try_into()?;
                    // Preloading a non-existing account will succeed, for now
                    proxy.preload(&pre_auth_state).await??;
                    assert_eq!(
                        proxy
                            .unlock_account(AccountHandlerControlUnlockAccountRequest::EMPTY)
                            .await?,
                        Err(ApiError::NotFound)
                    );
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_create_account_ephemeral_with_auth_mechanism() {
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(AccountLifetime::Ephemeral)
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    assert_eq!(
                        proxy
                            .create_account(AccountHandlerControlCreateAccountRequest {
                                id: Some(TEST_ACCOUNT_ID_UINT),
                                auth_mechanism_id: Some(TEST_AUTH_MECHANISM_ID.to_string()),
                                ..AccountHandlerControlCreateAccountRequest::EMPTY
                            })
                            .await?,
                        Err(ApiError::InvalidRequest)
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
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(true)
                .mechanisms(vec![Mechanism::Test])
                .test_fn(|proxy| async move {
                    assert_eq!(
                        proxy
                            .create_account(AccountHandlerControlCreateAccountRequest {
                                id: Some(TEST_ACCOUNT_ID_UINT),
                                ..AccountHandlerControlCreateAccountRequest::EMPTY
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
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(true)
                .mechanisms(vec![Mechanism::Test, Mechanism::Password])
                .test_fn(|proxy| async move {
                    assert_eq!(
                        proxy
                            .create_account(AccountHandlerControlCreateAccountRequest {
                                id: Some(TEST_ACCOUNT_ID_UINT),
                                ..AccountHandlerControlCreateAccountRequest::EMPTY
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
                .inspector(Arc::new(Inspector::new()))
                .is_interaction_enabled(true)
                .mechanisms(vec![])
                .test_fn(|proxy| async move {
                    assert_eq!(
                        proxy
                            .create_account(AccountHandlerControlCreateAccountRequest {
                                id: Some(TEST_ACCOUNT_ID_UINT),
                                ..AccountHandlerControlCreateAccountRequest::EMPTY
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
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(AccountLifetime::Ephemeral)
                .inspector(Arc::clone(&inspector))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|account_handler_proxy| async move {
                    account_handler_proxy
                        .create_account(create_account_request(TEST_ACCOUNT_ID_UINT))
                        .await??;

                    // Get a proxy to the Account interface
                    let (account_client_end, account_server_end) = create_endpoints().unwrap();
                    account_handler_proxy.get_account(account_server_end).await??;
                    let account_proxy = account_client_end.into_proxy().unwrap();

                    // Send the lock request
                    assert_eq!(account_proxy.lock().await?, Err(ApiError::FailedPrecondition));

                    // Wait for a potentitially faulty lock request to propagate
                    fasync::Timer::new(LOCK_REQUEST_DURATION.after_now()).await;

                    // The channel is still usable
                    assert!(account_proxy.get_persona_ids().await.is_ok());

                    // The state remains initialized
                    assert_data_tree!(inspector, root: {
                        account_handler: contains {
                            lifecycle: "initialized",
                        }
                    });
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_lock_request_persistent_account_without_auth_mechanism() {
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(location.to_persistent_lifetime())
                .inspector(Arc::clone(&inspector))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|account_handler_proxy| async move {
                    account_handler_proxy
                        .create_account(create_account_request(TEST_ACCOUNT_ID_UINT))
                        .await??;

                    // Get a proxy to the Account interface
                    let (account_client_end, account_server_end) = create_endpoints().unwrap();
                    account_handler_proxy.get_account(account_server_end).await??;
                    let account_proxy = account_client_end.into_proxy().unwrap();

                    // Send the lock request
                    assert_eq!(account_proxy.lock().await?, Err(ApiError::FailedPrecondition));
                    Ok(())
                })
                .build(),
        );
    }

    #[test]
    fn test_create_account_without_id() {
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            RequestStreamTestArgs::builder()
                .lifetime(AccountLifetime::Ephemeral)
                .inspector(Arc::clone(&inspector))
                .is_interaction_enabled(false)
                .mechanisms(vec![])
                .test_fn(|account_handler_proxy| async move {
                    // Send the invalid request
                    assert_eq!(
                        account_handler_proxy
                            .create_account(AccountHandlerControlCreateAccountRequest::EMPTY)
                            .await?,
                        Err(ApiError::InvalidRequest)
                    );

                    Ok(())
                })
                .build(),
        );
    }
}
