// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        account_metadata::AuthenticatorMetadata,
        password_interaction::{PasswordInteractionHandler, Validator},
        pinweaver::{CredManagerProvider, EnrollPinweaverValidator},
        scrypt::EnrollScryptValidator,
        Config,
    },
    anyhow::anyhow,
    fidl_fuchsia_identity_authentication::{
        AttemptedEvent, Enrollment, Error as ApiError, InteractionProtocolServerEnd,
        StorageUnlockMechanismRequest, StorageUnlockMechanismRequestStream,
    },
    futures::TryStreamExt,
    identity_common::{EnrollmentData, PrekeyMaterial},
    tracing::log::error,
};

/// A struct to handle authentication and enrollment requests.
pub struct StorageUnlockMechanism<CMP>
where
    CMP: CredManagerProvider + 'static,
{
    config: Config,
    cred_manager_provider: CMP,
}

impl<CMP> StorageUnlockMechanism<CMP>
where
    CMP: CredManagerProvider + 'static,
{
    /// Instantiate a new StorageUnlockMechanism.
    pub fn new(config: Config, cred_manager_provider: CMP) -> Self {
        Self { config, cred_manager_provider }
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
                responder.send(&mut self.authenticate(interaction, enrollments))
            }
            StorageUnlockMechanismRequest::Enroll { interaction, responder } => {
                match self.enroll(interaction).await {
                    Ok((enrollment_data, prekey)) => {
                        responder.send(&mut Ok((enrollment_data.0, prekey.0)))
                    }
                    Err(e) => responder.send(&mut Err(e)),
                }
            }
        }
    }

    fn authenticate(
        &self,
        _interaction: InteractionProtocolServerEnd,
        _enrollments: Vec<Enrollment>,
    ) -> Result<AttemptedEvent, ApiError> {
        unimplemented!()
    }

    async fn enroll(
        &self,
        interaction: InteractionProtocolServerEnd,
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

        let server_end = if let InteractionProtocolServerEnd::Password(server_end) = interaction {
            server_end
        } else {
            return Err(ApiError::InvalidRequest);
        };

        let stream = server_end.into_stream().map_err(|_| ApiError::Resource)?;
        let (metadata, key) = PasswordInteractionHandler::new(validator)
            .await
            .handle_password_interaction_request_stream(stream)
            .await?;

        let enrollment_data =
            EnrollmentData(serde_json::to_vec(&metadata).map_err(|_| ApiError::InvalidDataFormat)?);
        Ok((enrollment_data, key))
    }
}

#[cfg(test)]
mod test {
    use {
        super::*,
        crate::{
            keys::KEY_LEN,
            pinweaver::{
                MockCredManager, MockCredManagerProvider, PinweaverParams,
                TEST_PINWEAVER_CREDENTIAL_LABEL, TEST_PINWEAVER_LE_SECRET,
            },
            scrypt::TEST_SCRYPT_PASSWORD,
        },
        anyhow::Result,
        assert_matches::assert_matches,
        fidl_fuchsia_identity_authentication::{PasswordInteractionMarker, TestInteractionMarker},
        futures::StreamExt,
    };

    // By default, allow any implemented form of password and encryption.
    const DEFAULT_CONFIG: Config = Config { allow_scrypt: true, allow_pinweaver: true };
    // Define more restrictive configs to verify exclusions are implemented correctly.
    const SCRYPT_ONLY_CONFIG: Config = Config { allow_scrypt: true, allow_pinweaver: false };
    const PINWEAVER_ONLY_CONFIG: Config = Config { allow_scrypt: false, allow_pinweaver: true };

    async fn pinweaver_success_case(config: Config) -> Result<()> {
        let mcm = MockCredManager::new_expect_le_secret(&TEST_PINWEAVER_LE_SECRET);
        let initial_creds = mcm.get_number_of_creds();
        let cred_manager_provider = MockCredManagerProvider::new_with_cred_manager(mcm.clone());
        let storage_unlock_mechanism = StorageUnlockMechanism::new(config, cred_manager_provider);
        let (client, interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>().unwrap();

        let _ = client.into_proxy().unwrap().set_password(TEST_SCRYPT_PASSWORD).unwrap();

        let result = storage_unlock_mechanism
            .enroll(InteractionProtocolServerEnd::Password(interaction_server))
            .await;
        assert!(result.is_ok());
        let (enrollment_data, prekey) = result.unwrap();
        assert_eq!(prekey.0.len(), KEY_LEN);

        let str_enrollment = std::str::from_utf8(&enrollment_data.0).unwrap();
        let authenticator_data: AuthenticatorMetadata =
            serde_json::from_str(str_enrollment).unwrap();

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
        Ok(())
    }

    #[fuchsia::test]
    #[should_panic(expected = "not implemented")]
    async fn test_authenticate_not_implemented() {
        let cred_manager_provider = MockCredManagerProvider::new();
        let storage_unlock_mechanism =
            StorageUnlockMechanism::new(DEFAULT_CONFIG, cred_manager_provider);
        let (_, interaction_server) =
            fidl::endpoints::create_endpoints::<TestInteractionMarker>().unwrap();

        let _ = storage_unlock_mechanism
            .authenticate(InteractionProtocolServerEnd::Test(interaction_server), vec![]);
    }

    #[fuchsia::test]
    async fn test_enroll_scrypt() {
        let cred_manager_provider = MockCredManagerProvider::new();
        let storage_unlock_mechanism =
            StorageUnlockMechanism::new(SCRYPT_ONLY_CONFIG, cred_manager_provider);
        let (client, interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>().unwrap();

        let _ = client.into_proxy().unwrap().set_password("password").unwrap();

        let result = storage_unlock_mechanism
            .enroll(InteractionProtocolServerEnd::Password(interaction_server))
            .await;
        assert!(result.is_ok());
    }

    #[fuchsia::test]
    async fn test_enroll_pinweaver_success() -> Result<()> {
        pinweaver_success_case(PINWEAVER_ONLY_CONFIG).await
    }

    #[fuchsia::test]
    async fn test_enroll_pinweaver_failure() {
        let mcm = MockCredManager::new_fail_enrollment();
        let cred_manager_provider = MockCredManagerProvider::new_with_cred_manager(mcm);
        let storage_unlock_mechanism =
            StorageUnlockMechanism::new(PINWEAVER_ONLY_CONFIG, cred_manager_provider);
        let (client, interaction_server) =
            fidl::endpoints::create_endpoints::<PasswordInteractionMarker>().unwrap();

        let password_proxy = client.into_proxy().unwrap();
        let _ = password_proxy.set_password(TEST_SCRYPT_PASSWORD).unwrap();

        let result = storage_unlock_mechanism
            .enroll(InteractionProtocolServerEnd::Password(interaction_server))
            .await;
        assert_matches!(result, Err(ApiError::Resource));
        assert_matches!(
            password_proxy.take_event_stream().next().await.unwrap(),
            Err(fidl::Error::ClientChannelClosed { status: fidl::Status::BAD_STATE, .. })
        );
    }

    #[fuchsia::test]
    async fn test_enroll_pinweaver_and_scrypt() -> Result<()> {
        pinweaver_success_case(DEFAULT_CONFIG).await
    }

    #[fuchsia::test]
    async fn test_enroll_incorrect_ipse() {
        let cred_manager_provider = MockCredManagerProvider::new();
        let storage_unlock_mechanism =
            StorageUnlockMechanism::new(SCRYPT_ONLY_CONFIG, cred_manager_provider);
        let (client, interaction_server) =
            fidl::endpoints::create_endpoints::<TestInteractionMarker>().unwrap();

        let _ = client.into_proxy().unwrap().set_success().unwrap();

        let result = storage_unlock_mechanism
            .enroll(InteractionProtocolServerEnd::Test(interaction_server))
            .await;
        assert_eq!(result, Err(ApiError::InvalidRequest));
    }
}
