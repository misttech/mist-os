// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        account_metadata::{
            translate_from_enrollment, translate_to_enrollment, AuthenticatorMetadata,
        },
        password_interaction::{PasswordInteractionHandler, Validator},
        pinweaver::{
            AuthenticatePinweaverValidator, CredManagerProvider, EnrollPinweaverValidator,
        },
        scrypt::{AuthenticateScryptValidator, EnrollScryptValidator},
        Config,
    },
    anyhow::anyhow,
    fidl_fuchsia_identity_authentication::{
        AttemptedEvent, Enrollment, Error as ApiError, InteractionProtocolServerEnd,
        PasswordInteractionRequestStream, StorageUnlockMechanismRequest,
        StorageUnlockMechanismRequestStream,
    },
    fuchsia_zircon::Clock,
    futures::TryStreamExt,
    identity_common::{EnrollmentData, PrekeyMaterial},
    tracing::{error, info, warn},
};

/// A struct to handle authentication and enrollment requests.
pub struct StorageUnlockMechanism<CMP>
where
    CMP: CredManagerProvider + 'static,
{
    config: Config,
    cred_manager_provider: CMP,
    clock: Clock,
}

impl<CMP> StorageUnlockMechanism<CMP>
where
    CMP: CredManagerProvider + 'static,
{
    /// Instantiate a new StorageUnlockMechanism.
    pub fn new(config: Config, cred_manager_provider: CMP, clock: Clock) -> Self {
        Self { config, cred_manager_provider, clock }
    }

    /// Returns the time in the clock object, or None if time has not yet started.
    fn read_clock(&self) -> Option<fuchsia_zircon::Time> {
        match (self.clock.read(), self.clock.get_details()) {
            (Ok(time), Ok(details)) if time > details.backstop => Some(time),
            _ => None,
        }
    }

    /// Serially process a stream of incoming StorageUnlockMechanism FIDL requests.
    pub async fn handle_requests_for_storage_unlock_mechanism(
        &self,
        mut stream: StorageUnlockMechanismRequestStream,
    ) {
        // TODO(fxb/109121): Ensure that a client closing the channel does not cause all clients to fail.
        while let Some(request) = stream.try_next().await.expect("read request") {
            self.handle_storage_unlock_mechanism_request(request).await.unwrap_or_else(|e| {
                error!("error handling StorageUnlockMechanism fidl request: {:#}", anyhow!(e));
            })
        }
    }

    /// Process a single StorageUnlockMechanism FIDL request and send a reply.
    async fn handle_storage_unlock_mechanism_request(
        &self,
        request: StorageUnlockMechanismRequest,
    ) -> Result<(), fidl::Error> {
        match request {
            StorageUnlockMechanismRequest::Authenticate { interaction, enrollments, responder } => {
                let stream = match Self::stream_from_ipse(interaction) {
                    Ok(stream) => stream,
                    Err(e) => {
                        return responder.send(&mut Err(e));
                    }
                };
                match self.authenticate(stream, enrollments).await {
                    Ok(attempted_event) => responder.send(&mut Ok(attempted_event)),
                    Err(e) => responder.send(&mut Err(e)),
                }
            }
            StorageUnlockMechanismRequest::Enroll { interaction, responder } => {
                let stream = match Self::stream_from_ipse(interaction) {
                    Ok(stream) => stream,
                    Err(e) => {
                        return responder.send(Err(e));
                    }
                };

                match self.enroll(stream).await {
                    Ok((enrollment_data, prekey)) => {
                        responder.send(Ok((&enrollment_data.0, &prekey.0)))
                    }
                    Err(e) => responder.send(Err(e)),
                }
            }
        }
    }

    fn stream_from_ipse(
        ipse: InteractionProtocolServerEnd,
    ) -> Result<PasswordInteractionRequestStream, ApiError> {
        let server_end = if let InteractionProtocolServerEnd::Password(server_end) = ipse {
            server_end
        } else {
            return Err(ApiError::InvalidRequest);
        };

        server_end.into_stream().map_err(|_| ApiError::Resource)
    }

    async fn authenticate(
        &self,
        stream: PasswordInteractionRequestStream,
        enrollments: Vec<Enrollment>,
    ) -> Result<AttemptedEvent, ApiError> {
        // TODO(fxb/117412): Currently we only have one enrollment. When there are more,
        // change this constraint!
        if enrollments.len() != 1 {
            warn!("Error: Too many enrollments");
            return Err(ApiError::InvalidDataFormat);
        }

        let enrollment = &enrollments[0];

        let authenticator_data =
            translate_from_enrollment(enrollment).map_err(|_| ApiError::InvalidDataFormat)?;

        let validator: Box<dyn Validator<PrekeyMaterial>> = match authenticator_data {
            AuthenticatorMetadata::Pinweaver(p) => {
                if !self.config.allow_pinweaver {
                    warn!("Enrollment used pinweaver which is not supported by the current configuration");
                    return Err(ApiError::InvalidDataFormat);
                }
                let cred_manager = self
                    .cred_manager_provider
                    .new_cred_manager()
                    .map_err(|_| ApiError::Resource)?;
                Box::new(AuthenticatePinweaverValidator::new(cred_manager, p.pinweaver_params))
            }

            AuthenticatorMetadata::ScryptOnly(s) => {
                if !self.config.allow_scrypt {
                    warn!("Enrollment used scrypt which is not supported by the current configuration");
                    return Err(ApiError::InvalidDataFormat);
                }
                Box::new(AuthenticateScryptValidator::new(s))
            }
        };

        let key = PasswordInteractionHandler::new(validator)
            .await
            .handle_password_interaction_request_stream(stream)
            .await?;

        info!(
            "Successfully generated authentication AttemptedEvent using {}",
            authenticator_data.flavor()
        );
        Ok(AttemptedEvent {
            timestamp: self.read_clock().map(|time| time.into_nanos()),
            enrollment_id: Some(enrollment.id),
            updated_enrollment_data: None,
            prekey_material: Some(key.0),
            ..Default::default()
        })
    }

    async fn enroll(
        &self,
        stream: PasswordInteractionRequestStream,
    ) -> Result<(EnrollmentData, PrekeyMaterial), ApiError> {
        let validator: Box<dyn Validator<(AuthenticatorMetadata, PrekeyMaterial)>> = if self
            .config
            .allow_pinweaver
        {
            let cred_manager =
                self.cred_manager_provider.new_cred_manager().map_err(|_| ApiError::Resource)?;
            Box::new(EnrollPinweaverValidator::new(cred_manager))
        } else {
            Box::new(EnrollScryptValidator {})
        };

        let (metadata, key) = PasswordInteractionHandler::new(validator)
            .await
            .handle_password_interaction_request_stream(stream)
            .await?;
        let enrollment_data =
            translate_to_enrollment(metadata).map_err(|_| ApiError::InvalidDataFormat)?;
        info!("Successfully completed enrollment using {}", metadata.flavor());
        Ok((enrollment_data, key))
    }
}

#[cfg(test)]
mod test {
    use {
        super::*,
        crate::{
            account_metadata::{PinweaverMetadata, ScryptOnlyMetadata},
            keys::KEY_LEN,
            pinweaver::{
                MockCredManager, MockCredManagerProvider, PinweaverParams,
                TEST_PINWEAVER_CREDENTIAL_LABEL, TEST_PINWEAVER_LE_SECRET,
            },
            scrypt::{TEST_SCRYPT_PARAMS, TEST_SCRYPT_PASSWORD},
        },
        anyhow::Result,
        assert_matches::assert_matches,
        fidl_fuchsia_identity_authentication::{PasswordInteractionMarker, TestInteractionMarker},
        fuchsia_zircon as zx,
        futures::StreamExt,
    };

    // By default, allow any implemented form of password and encryption.
    const DEFAULT_CONFIG: Config = Config { allow_scrypt: true, allow_pinweaver: true };
    // Define more restrictive configs to verify exclusions are implemented correctly.
    const SCRYPT_ONLY_CONFIG: Config = Config { allow_scrypt: true, allow_pinweaver: false };
    const PINWEAVER_ONLY_CONFIG: Config = Config { allow_scrypt: false, allow_pinweaver: true };

    const TEST_ENROLLMENT_ID: u64 = 0x99;

    fn create_storage_unlock_with_default(
        config: Config,
    ) -> StorageUnlockMechanism<MockCredManagerProvider> {
        create_storage_unlock_with_custom_cmp(config, MockCredManagerProvider::new())
    }

    fn create_storage_unlock_with_custom_cmp(
        config: Config,
        cmp: MockCredManagerProvider,
    ) -> StorageUnlockMechanism<MockCredManagerProvider> {
        let clock = zx::Clock::create(zx::ClockOpts::empty(), None).unwrap();
        StorageUnlockMechanism::new(config, cmp, clock)
    }

    async fn enroll_pinweaver_success_case(
        config: Config,
    ) -> Result<(MockCredManagerProvider, Enrollment, PrekeyMaterial)> {
        let mcm = MockCredManager::new_expect_le_secret(&TEST_PINWEAVER_LE_SECRET);
        let initial_creds = mcm.get_number_of_creds();
        let cred_manager_provider = MockCredManagerProvider::new_with_cred_manager(mcm.clone());
        let storage_unlock_mechanism =
            create_storage_unlock_with_custom_cmp(config, cred_manager_provider.clone());
        let (client, interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();

        let _ = client.into_proxy().unwrap().set_password(TEST_SCRYPT_PASSWORD).unwrap();
        let enroll_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(interaction_server),
        )
        .unwrap();

        let result = storage_unlock_mechanism.enroll(enroll_stream).await;
        assert!(result.is_ok());
        let (enrollment_data, prekey) = result.unwrap();
        assert_eq!(prekey.0.len(), KEY_LEN);

        let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enrollment_data.0 };
        let authenticator_data: AuthenticatorMetadata =
            translate_from_enrollment(&enrollment).unwrap();

        let pinweaver_params = if let AuthenticatorMetadata::Pinweaver(p) = authenticator_data {
            p.pinweaver_params
        } else {
            return Err(anyhow!("wrong authenticator metadata type"));
        };

        assert_matches!(
            pinweaver_params,
            PinweaverParams { scrypt_params: _, credential_label: TEST_PINWEAVER_CREDENTIAL_LABEL }
        );

        let final_creds = mcm.get_number_of_creds();
        assert!(final_creds > initial_creds);
        Ok((cred_manager_provider, enrollment, prekey))
    }

    async fn authenticate_pinweaver_success(
        config: Config,
        cmp: MockCredManagerProvider,
        enrollment: Enrollment,
        enroll_key: PrekeyMaterial,
    ) -> Result<()> {
        let auth_storage_unlock_mechanism = create_storage_unlock_with_custom_cmp(config, cmp);
        let (client, auth_interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();

        let _ = client.into_proxy().unwrap().set_password(TEST_SCRYPT_PASSWORD).unwrap();

        let auth_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(auth_interaction_server),
        )
        .unwrap();

        let attempted_event = auth_storage_unlock_mechanism
            .authenticate(auth_stream, vec![enrollment])
            .await
            .unwrap();

        assert_eq!(attempted_event.enrollment_id, Some(TEST_ENROLLMENT_ID));
        assert_eq!(attempted_event.updated_enrollment_data, None);
        assert_eq!(attempted_event.prekey_material, Some(enroll_key.0));

        Ok(())
    }

    #[fuchsia::test]
    async fn test_authenticate_scrypt() {
        let storage_unlock_mechanism = create_storage_unlock_with_default(SCRYPT_ONLY_CONFIG);

        let meta = AuthenticatorMetadata::ScryptOnly(ScryptOnlyMetadata {
            scrypt_params: TEST_SCRYPT_PARAMS,
        });
        let data = serde_json::to_vec(&meta).unwrap();

        let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: data.clone() };

        let (client, auth_interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();

        let _ = client.into_proxy().unwrap().set_password("password").unwrap();
        let auth_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(auth_interaction_server),
        )
        .unwrap();

        let attempted_event =
            storage_unlock_mechanism.authenticate(auth_stream, vec![enrollment]).await.unwrap();

        assert_eq!(attempted_event.enrollment_id, Some(TEST_ENROLLMENT_ID));
        assert_eq!(attempted_event.updated_enrollment_data, None);
        assert_eq!(attempted_event.timestamp, None);
    }

    #[fuchsia::test]
    async fn test_authenticate_scrypt_with_custom_clock() {
        let backstop = zx::Time::from_nanos(5500);
        let clock =
            zx::Clock::create(zx::ClockOpts::AUTO_START | zx::ClockOpts::MONOTONIC, Some(backstop))
                .unwrap();
        let storage_unlock_mechanism =
            StorageUnlockMechanism::new(SCRYPT_ONLY_CONFIG, MockCredManagerProvider::new(), clock);

        let meta = AuthenticatorMetadata::ScryptOnly(ScryptOnlyMetadata {
            scrypt_params: TEST_SCRYPT_PARAMS,
        });
        let data = serde_json::to_vec(&meta).unwrap();

        let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: data.clone() };

        let (client, auth_interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();

        let _ = client.into_proxy().unwrap().set_password("password").unwrap();
        let auth_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(auth_interaction_server),
        )
        .unwrap();

        let attempted_event =
            storage_unlock_mechanism.authenticate(auth_stream, vec![enrollment]).await.unwrap();

        assert_eq!(attempted_event.enrollment_id, Some(TEST_ENROLLMENT_ID));
        assert_eq!(attempted_event.updated_enrollment_data, None);
        assert!(attempted_event.timestamp.unwrap() > backstop.into_nanos());
    }

    #[fuchsia::test]
    async fn test_authenticate_pinweaver_invalid_label_error() {
        let storage_unlock_mechanism = create_storage_unlock_with_default(PINWEAVER_ONLY_CONFIG);

        let meta = AuthenticatorMetadata::Pinweaver(PinweaverMetadata {
            pinweaver_params: PinweaverParams {
                scrypt_params: TEST_SCRYPT_PARAMS,
                credential_label: TEST_PINWEAVER_CREDENTIAL_LABEL,
            },
        });
        let data = serde_json::to_vec(&meta).unwrap();

        let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: data.clone() };

        let (client, auth_interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();

        let _ = client.into_proxy().unwrap().set_password("password").unwrap();
        let auth_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(auth_interaction_server),
        )
        .unwrap();

        let result = storage_unlock_mechanism.authenticate(auth_stream, vec![enrollment]).await;

        assert_eq!(result, Err(ApiError::Resource));
    }

    #[fuchsia::test]
    async fn test_enroll_scrypt_authenticate_pinweaver_error() {
        let enroll_storage_unlock_mechanism =
            create_storage_unlock_with_default(SCRYPT_ONLY_CONFIG);
        let (client, interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();

        let _ = client.into_proxy().unwrap().set_password("password").unwrap();
        let enroll_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(interaction_server),
        )
        .unwrap();

        let (enroll_data, _) = enroll_storage_unlock_mechanism.enroll(enroll_stream).await.unwrap();

        let (client, auth_interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();
        let _ = client.into_proxy().unwrap().set_password("password").unwrap();

        let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enroll_data.0 };
        let auth_storage_unlock_mechanism =
            create_storage_unlock_with_default(PINWEAVER_ONLY_CONFIG);
        let auth_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(auth_interaction_server),
        )
        .unwrap();

        let result =
            auth_storage_unlock_mechanism.authenticate(auth_stream, vec![enrollment]).await;
        assert_eq!(result, Err(ApiError::InvalidDataFormat));
    }

    #[fuchsia::test]
    async fn test_enroll_pinweaver_authenticate_scrypt_error() {
        let (_, enrollment, _) =
            enroll_pinweaver_success_case(PINWEAVER_ONLY_CONFIG).await.unwrap();

        let (client, auth_interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();
        let _ = client.into_proxy().unwrap().set_password(TEST_SCRYPT_PASSWORD).unwrap();

        let auth_storage_unlock_mechanism = create_storage_unlock_with_default(SCRYPT_ONLY_CONFIG);
        let auth_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(auth_interaction_server),
        )
        .unwrap();

        let result =
            auth_storage_unlock_mechanism.authenticate(auth_stream, vec![enrollment]).await;
        assert_eq!(result, Err(ApiError::InvalidDataFormat));
    }

    #[fuchsia::test]
    async fn test_enroll_scrypt_authenticate_both_config() {
        let enroll_storage_unlock_mechanism =
            create_storage_unlock_with_default(SCRYPT_ONLY_CONFIG);
        let (client, interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();

        let _ = client.into_proxy().unwrap().set_password("password").unwrap();
        let enroll_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(interaction_server),
        )
        .unwrap();

        let (enroll_data, enroll_key) =
            enroll_storage_unlock_mechanism.enroll(enroll_stream).await.unwrap();

        let (client, auth_interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();
        let _ = client.into_proxy().unwrap().set_password("password").unwrap();

        let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enroll_data.0 };
        let auth_storage_unlock_mechanism = create_storage_unlock_with_default(DEFAULT_CONFIG);

        let auth_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(auth_interaction_server),
        )
        .unwrap();

        let attempted_event = auth_storage_unlock_mechanism
            .authenticate(auth_stream, vec![enrollment])
            .await
            .unwrap();
        assert_eq!(attempted_event.enrollment_id, Some(TEST_ENROLLMENT_ID));
        assert_eq!(attempted_event.updated_enrollment_data, None);
        assert_eq!(attempted_event.prekey_material, Some(enroll_key.0));
    }

    #[fuchsia::test]
    async fn test_enroll_pinweaver_authenticate_both_config() -> Result<()> {
        let (cmp, enrollment, enroll_prekey) =
            enroll_pinweaver_success_case(PINWEAVER_ONLY_CONFIG).await.unwrap();
        authenticate_pinweaver_success(DEFAULT_CONFIG, cmp, enrollment, enroll_prekey).await
    }

    #[fuchsia::test]
    async fn test_enroll_default_authenticate_pinweaver() -> Result<()> {
        let (cmp, enrollment, enroll_prekey) =
            enroll_pinweaver_success_case(DEFAULT_CONFIG).await.unwrap();
        authenticate_pinweaver_success(PINWEAVER_ONLY_CONFIG, cmp, enrollment, enroll_prekey).await
    }

    #[fuchsia::test]
    async fn test_pinweaver_roundtrip_success() -> Result<()> {
        let (cmp, enrollment, enroll_prekey) =
            enroll_pinweaver_success_case(PINWEAVER_ONLY_CONFIG).await.unwrap();
        authenticate_pinweaver_success(PINWEAVER_ONLY_CONFIG, cmp, enrollment, enroll_prekey).await
    }

    #[fuchsia::test]
    async fn test_pinweaver_authenticate_wrong_password_raises_resource_error() {
        let (cmp, enrollment, _) =
            enroll_pinweaver_success_case(PINWEAVER_ONLY_CONFIG).await.unwrap();

        let auth_storage_unlock_mechanism =
            create_storage_unlock_with_custom_cmp(PINWEAVER_ONLY_CONFIG, cmp);
        let (client, auth_interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();

        let _ = client.into_proxy().unwrap().set_password("wrong password").unwrap();
        let auth_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(auth_interaction_server),
        )
        .unwrap();

        let result =
            auth_storage_unlock_mechanism.authenticate(auth_stream, vec![enrollment]).await;
        assert_eq!(result, Err(ApiError::Resource));
    }

    #[fuchsia::test]
    async fn test_scrypt_roundtrip() {
        let storage_unlock_mechanism = create_storage_unlock_with_default(SCRYPT_ONLY_CONFIG);
        let (client, interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();

        let _ = client.into_proxy().unwrap().set_password(TEST_SCRYPT_PASSWORD).unwrap();
        let stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(interaction_server),
        )
        .unwrap();

        let (enroll_data, enroll_key) = storage_unlock_mechanism.enroll(stream).await.unwrap();

        let (client, auth_interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();
        let _ = client.into_proxy().unwrap().set_password(TEST_SCRYPT_PASSWORD).unwrap();
        let auth_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(auth_interaction_server),
        )
        .unwrap();

        let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enroll_data.0 };

        let attempted_event =
            storage_unlock_mechanism.authenticate(auth_stream, vec![enrollment]).await.unwrap();

        assert_eq!(attempted_event.enrollment_id, Some(TEST_ENROLLMENT_ID));
        assert_eq!(attempted_event.updated_enrollment_data, None);
        assert_eq!(attempted_event.prekey_material, Some(enroll_key.0));
    }

    #[fuchsia::test]
    async fn test_scrypt_roundtrip_different_passwords() {
        let storage_unlock_mechanism = create_storage_unlock_with_default(SCRYPT_ONLY_CONFIG);
        let (client, interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();

        let _ = client.into_proxy().unwrap().set_password("password").unwrap();
        let stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(interaction_server),
        )
        .unwrap();

        let (enroll_data, enroll_key) = storage_unlock_mechanism.enroll(stream).await.unwrap();

        let (client, auth_interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();
        let _ = client.into_proxy().unwrap().set_password("wrong password").unwrap();

        let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enroll_data.0 };
        let auth_stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(auth_interaction_server),
        )
        .unwrap();

        let attempted_event =
            storage_unlock_mechanism.authenticate(auth_stream, vec![enrollment]).await.unwrap();

        assert_eq!(attempted_event.enrollment_id, Some(TEST_ENROLLMENT_ID));
        assert_eq!(attempted_event.updated_enrollment_data, None);
        assert_ne!(attempted_event.prekey_material, Some(enroll_key.0));
    }

    #[fuchsia::test]
    async fn test_enroll_scrypt() {
        let storage_unlock_mechanism = create_storage_unlock_with_default(SCRYPT_ONLY_CONFIG);
        let (client, interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();

        let _ = client.into_proxy().unwrap().set_password("password").unwrap();
        let stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(interaction_server),
        )
        .unwrap();

        let result = storage_unlock_mechanism.enroll(stream).await;
        assert!(result.is_ok());
    }

    #[fuchsia::test]
    async fn test_enroll_pinweaver_success() {
        assert!(enroll_pinweaver_success_case(PINWEAVER_ONLY_CONFIG).await.is_ok())
    }

    #[fuchsia::test]
    async fn test_enroll_pinweaver_failure() {
        let mcm = MockCredManager::new_fail_enrollment();
        let cred_manager_provider = MockCredManagerProvider::new_with_cred_manager(mcm);
        let storage_unlock_mechanism =
            create_storage_unlock_with_custom_cmp(PINWEAVER_ONLY_CONFIG, cred_manager_provider);
        let (client, interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>();

        let password_proxy = client.into_proxy().unwrap();
        let _ = password_proxy.set_password(TEST_SCRYPT_PASSWORD).unwrap();

        let stream = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Password(interaction_server),
        )
        .unwrap();

        let result = storage_unlock_mechanism.enroll(stream).await;
        assert_matches!(result, Err(ApiError::Resource));
        assert_matches!(
            password_proxy.take_event_stream().next().await.unwrap(),
            Err(fidl::Error::ClientChannelClosed { status: fidl::Status::BAD_STATE, .. })
        );
    }

    #[fuchsia::test]
    async fn test_enroll_pinweaver_and_scrypt() {
        assert!(enroll_pinweaver_success_case(DEFAULT_CONFIG).await.is_ok())
    }

    #[fuchsia::test]
    async fn test_enroll_incorrect_ipse() {
        let (client, interaction_server) =
            fidl::endpoints::create_endpoints::<TestInteractionMarker>();

        let _ = client.into_proxy().unwrap().set_success().unwrap();
        let result = StorageUnlockMechanism::<MockCredManagerProvider>::stream_from_ipse(
            InteractionProtocolServerEnd::Test(interaction_server),
        );

        assert!(result.is_err());
    }
}
