// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::test_interaction::TestInteraction,
    fidl_fuchsia_identity_authentication::{
        AttemptedEvent, Enrollment, Error as ApiError, InteractionProtocolServerEnd,
        StorageUnlockMechanismRequest, StorageUnlockMechanismRequestStream,
    },
    fuchsia_runtime::duplicate_utc_clock_handle,
    fuchsia_zircon::{self as zx, Clock},
    futures::prelude::*,
    identity_common::{EnrollmentData, PrekeyMaterial},
    lazy_static::lazy_static,
    tracing::warn,
};

lazy_static! {
    /// The enrollment data always reported by this authenticator.
    static ref FIXED_ENROLLMENT_DATA: EnrollmentData = EnrollmentData(vec![0, 1, 2]);

    /// The magic prekey material corresponding to a successful authentication
    /// attempt with the account system. This constant is copied to
    /// the account_handler implementation and needs to stay in sync.
    static ref MAGIC_PREKEY: PrekeyMaterial = PrekeyMaterial(vec![77; 32]);

    /// Valid prekey material but is not the magic prekey. Should be used to
    /// generate authentication failures.
    static ref NOT_MAGIC_PREKEY: PrekeyMaterial = PrekeyMaterial(vec![80; 32]);
}

/// A development-only implementation of the
/// fuchsia.identity.authentication.StorageUnlockMechanism fidl protocol.
pub struct StorageUnlockMechanism {
    clock: Clock,
}

impl StorageUnlockMechanism {
    pub fn new() -> Self {
        Self {
            clock: duplicate_utc_clock_handle(zx::Rights::SAME_RIGHTS)
                .expect("Failed to duplicate UTC clock handle."),
        }
    }

    /// Returns the time in the clock object, or None if time has not yet started.
    fn read_clock(&self) -> Option<zx::Time> {
        match (self.clock.read(), self.clock.get_details()) {
            (Ok(time), Ok(details)) if time > details.backstop => Some(time),
            _ => None,
        }
    }

    /// Asynchronously handle fidl requests received on the provided stream.
    pub async fn handle_requests_from_stream(
        &self,
        mut stream: StorageUnlockMechanismRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Some(request) = stream.try_next().await? {
            self.handle_request(request).await?;
        }
        Ok(())
    }

    /// Asynchronously handle a fidl request.
    async fn handle_request(
        &self,
        request: StorageUnlockMechanismRequest,
    ) -> Result<(), fidl::Error> {
        match request {
            StorageUnlockMechanismRequest::Authenticate { interaction, enrollments, responder } => {
                let mut response = self.authenticate(interaction, enrollments).await;
                responder.send(&mut response)
            }
            StorageUnlockMechanismRequest::Enroll { interaction, responder } => {
                responder.send(self.enroll(interaction).await)
            }
        }
    }

    /// Implementation of `authenticate` fidl method.
    async fn authenticate(
        &self,
        interaction: InteractionProtocolServerEnd,
        enrollments: Vec<Enrollment>,
    ) -> Result<AttemptedEvent, ApiError> {
        let test_interaction_server_end =
            if let InteractionProtocolServerEnd::Test(server_end) = interaction {
                server_end
            } else {
                warn!("Unsupported InteractionProtocolServerEnd: Only Test is supported");
                return Err(ApiError::InvalidRequest);
            };

        let prekey_material = if TestInteraction::start(test_interaction_server_end).await? {
            MAGIC_PREKEY.clone()
        } else {
            // If the client closed the TestInteraction channel, it
            // means there won't be any retries and the client has
            // given up on a successful result and aborted the attempt.
            warn!("TestInteraction channel closed. Aborting authentication.");
            return Err(ApiError::Aborted);
        };

        // Take the ID of the first enrollment.
        let enrollment = enrollments.into_iter().next().ok_or(ApiError::InvalidRequest)?;
        let Enrollment { id, .. } = enrollment;

        Ok(AttemptedEvent {
            timestamp: self.read_clock().map(|time| time.into_nanos()),
            enrollment_id: Some(id),
            updated_enrollment_data: None,
            prekey_material: Some(prekey_material.to_vec()),
            ..Default::default()
        })
    }

    /// Implementation of `enroll` fidl method.
    async fn enroll(
        &self,
        interaction: InteractionProtocolServerEnd,
    ) -> Result<(&[u8], &[u8]), ApiError> {
        match interaction {
            InteractionProtocolServerEnd::Test(_) => Ok((&FIXED_ENROLLMENT_DATA, &MAGIC_PREKEY)),
            _ => {
                warn!("Unsupported InteractionProtocolServerEnd: Only Test is supported");
                Err(ApiError::InvalidRequest)
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_endpoints, create_proxy, create_proxy_and_stream};
    use fidl_fuchsia_identity_authentication::{
        InteractionProtocolServerEnd, StorageUnlockMechanismMarker, StorageUnlockMechanismProxy,
        TestInteractionProxy,
    };
    use fuchsia_async::{Duration, DurationExt, TimeoutExt};
    use futures::future::join;

    const TEST_ENROLLMENT_ID: u64 = 0x42;
    const TEST_ENROLLMENT_ID_2: u64 = 0xabba;

    const TEST_TIMEOUT: Duration = Duration::from_seconds(2);

    fn create_test_interaction_protocol() -> InteractionProtocolServerEnd {
        let (_, server_end) = create_endpoints();
        InteractionProtocolServerEnd::Test(server_end)
    }

    fn create_test_interaction_proxy_and_server_end(
    ) -> (TestInteractionProxy, InteractionProtocolServerEnd) {
        let (proxy, server_end) = create_proxy().unwrap();
        (proxy, InteractionProtocolServerEnd::Test(server_end))
    }

    fn create_password_interaction_protocol() -> InteractionProtocolServerEnd {
        let (_, server_end) = create_endpoints();
        InteractionProtocolServerEnd::Password(server_end)
    }

    /// Starts the StorageUnlockMechanism server. Then it runs the `test_fn` and
    /// checks its result. Also asserts that the server returns an `Ok` value.
    async fn run_proxy_test<Fn, Fut>(test_fn: Fn)
    where
        Fn: FnOnce(StorageUnlockMechanismProxy) -> Fut,
        Fut: Future<Output = Result<(), fidl::Error>>,
    {
        let (proxy, stream) = create_proxy_and_stream::<StorageUnlockMechanismMarker>().unwrap();
        let mechanism = StorageUnlockMechanism::new();
        let server_fut = mechanism.handle_requests_from_stream(stream);
        let test_fut = test_fn(proxy);

        let (test_result, server_result) = join(test_fut, server_fut).await;
        assert!(test_result.is_ok());
        assert!(server_result.is_ok());
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn no_set_success_authentication_enroll_and_authenticate() {
        run_proxy_test(|proxy| async move {
            let (enrollment_data, enrollment_prekey) =
                proxy.enroll(create_test_interaction_protocol()).await?.unwrap();

            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enrollment_data.clone() };
            assert_eq!(enrollment_prekey, MAGIC_PREKEY.0);

            let (test_proxy, test_ipse) = create_test_interaction_proxy_and_server_end();
            std::mem::drop(test_proxy); // Drop test_proxy without calling SetSuccess().

            let auth_result = proxy.authenticate(test_ipse, &[enrollment]).await?;

            // Should return "Aborted" since we closed the channel without
            // calling SetSuccess().
            assert_eq!(auth_result, Err(ApiError::Aborted));
            Ok(())
        })
        .await
    }

    #[fuchsia::test(allow_stalls = true)]
    async fn no_set_success_authentication_enroll_and_authenticate_timeout() {
        // Since we don't call SetSuccess on the TestInteraction channel, the
        // TestInteraction server handler, and hence the StorageUnlockMechanism
        // server handler will keep waiting and will not return.
        run_proxy_test(|proxy| async move {
            let (enrollment_data, enrollment_prekey) =
                proxy.enroll(create_test_interaction_protocol()).await?.unwrap();

            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enrollment_data.clone() };
            assert_eq!(enrollment_prekey, MAGIC_PREKEY.0);

            let (_test_proxy, test_ipse) = create_test_interaction_proxy_and_server_end();

            let mut auth_fut = proxy
                .authenticate(test_ipse, &[enrollment])
                .on_timeout(TEST_TIMEOUT.after_now(), || {
                    Err(fidl::Error::ServerRequestRead(zx::Status::TIMED_OUT))
                });

            assert!(futures::poll!(&mut auth_fut).is_pending());
            assert_matches!(
                auth_fut.await,
                Err(fidl::Error::ServerRequestRead(zx::Status::TIMED_OUT))
            );

            Ok(())
        })
        .await
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn interection_enroll_and_successfully_authenticate_produce_same_prekey() {
        run_proxy_test(|proxy| async move {
            let (enrollment_data, enrollment_prekey) =
                proxy.enroll(create_test_interaction_protocol()).await?.unwrap();

            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enrollment_data.clone() };
            let (test_proxy, test_ipse) = create_test_interaction_proxy_and_server_end();
            test_proxy.set_success()?;

            let AttemptedEvent { enrollment_id, updated_enrollment_data, prekey_material, .. } =
                proxy.authenticate(test_ipse, &[enrollment]).await?.unwrap();

            assert_eq!(enrollment_id, Some(TEST_ENROLLMENT_ID));
            assert!(updated_enrollment_data.is_none());
            assert_eq!(prekey_material, Some(enrollment_prekey));
            Ok(())
        })
        .await
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn set_success_and_authenticate_multiple_enrollments() {
        run_proxy_test(|proxy| async move {
            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: vec![3] };
            let enrollment_2 = Enrollment { id: TEST_ENROLLMENT_ID_2, data: vec![12] };
            let (test_proxy, test_ipse) = create_test_interaction_proxy_and_server_end();
            test_proxy.set_success()?;

            let AttemptedEvent { enrollment_id, updated_enrollment_data, prekey_material, .. } =
                proxy.authenticate(test_ipse, &[enrollment, enrollment_2]).await?.unwrap();

            assert_eq!(enrollment_id, Some(TEST_ENROLLMENT_ID));
            assert!(updated_enrollment_data.is_none());
            assert_eq!(prekey_material.as_ref(), Some(MAGIC_PREKEY.as_ref()));
            Ok(())
        })
        .await
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn password_ipse_fail_enrollment_and_authentication() {
        run_proxy_test(|proxy| async move {
            // Fail Enrollment since it only supports Test IPSE.
            assert_eq!(
                proxy.enroll(create_password_interaction_protocol()).await?,
                Err(ApiError::InvalidRequest)
            );

            let (enrollment_data, enrollment_prekey) =
                proxy.enroll(create_test_interaction_protocol()).await?.unwrap();

            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enrollment_data.clone() };
            assert_eq!(enrollment_prekey, MAGIC_PREKEY.0);

            // Fail authentication since it only supports Test IPSE.
            assert_eq!(
                proxy.authenticate(create_password_interaction_protocol(), &[enrollment]).await?,
                Err(ApiError::InvalidRequest)
            );

            Ok(())
        })
        .await
    }
}
