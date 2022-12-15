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
    lazy_static::lazy_static,
    tracing::warn,
};

type EnrollmentData = Vec<u8>;
type PrekeyMaterial = Vec<u8>;

lazy_static! {
    /// The enrollment data always reported by this authenticator.
    static ref FIXED_ENROLLMENT_DATA: Vec<u8> = vec![0, 1, 2];

    /// The magic prekey material corresponding to a successful authentication
    /// attempt with the account system. This constant is copied to
    /// the account_handler implementation and needs to stay in sync.
    static ref MAGIC_PREKEY: Vec<u8>  = vec![77; 32];

    /// Valid prekey material but is not the magic prekey. Should be used to
    /// generate authentication failures.
    static ref NOT_MAGIC_PREKEY: Vec<u8> = vec![80; 32];
}

/// Determines the behavior of the authenticator.
#[derive(Debug, Clone, Copy)]
pub enum Mode {
    /// Enroll returns fixed enrollment data and magic prekey material.
    /// Authenticate ignores enrollment data and returns magic prekey material.
    AlwaysSucceed,

    /// Enroll returns fixed enrollment data and magic prekey material.
    /// Authenticate ignores enrollment data and returns prekey material which
    /// is valid but not equal to the magic prekey.
    AlwaysFailAuthentication,

    /// Enroll returns fixed enrollment data and magic prekey material.
    /// Authenticate ignores enrollment data and returns magic prekey material
    /// if TestInteraction::SetSuccess() is called and a valid but different
    /// prekey otherwise.
    Interaction,
}

/// A development-only implementation of the
/// fuchsia.identity.authentication.StorageUnlockMechanism fidl protocol
/// that responds according to its `mode`.
pub struct StorageUnlockMechanism {
    mode: Mode,
    clock: Clock,
}

impl StorageUnlockMechanism {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
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
                let mut response = self.enroll(interaction).await;
                responder.send(&mut response)
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

        let prekey_material = match self.mode {
            Mode::AlwaysSucceed => MAGIC_PREKEY.clone(),
            Mode::AlwaysFailAuthentication => NOT_MAGIC_PREKEY.clone(),
            Mode::Interaction => {
                if TestInteraction::start(test_interaction_server_end).await? {
                    MAGIC_PREKEY.clone()
                } else {
                    // If the client closed the TestInteraction channel, it
                    // means there won't be any retries and the client has
                    // given up on a successful result and aborted the attempt.
                    warn!("TestInteraction channel closed. Aborting authentication.");
                    return Err(ApiError::Aborted);
                }
            }
        };

        // Take the ID of the first enrollment.
        let enrollment = enrollments.into_iter().next().ok_or(ApiError::InvalidRequest)?;
        let Enrollment { id, .. } = enrollment;

        Ok(AttemptedEvent {
            timestamp: self.read_clock().map(|time| time.into_nanos()),
            enrollment_id: Some(id),
            updated_enrollment_data: None,
            prekey_material: Some(prekey_material),
            ..AttemptedEvent::EMPTY
        })
    }

    /// Implementation of `enroll` fidl method.
    async fn enroll(
        &self,
        interaction: InteractionProtocolServerEnd,
    ) -> Result<(EnrollmentData, PrekeyMaterial), ApiError> {
        match interaction {
            InteractionProtocolServerEnd::Test(_) => {
                Ok((FIXED_ENROLLMENT_DATA.clone(), MAGIC_PREKEY.clone()))
            }
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
        let (_, server_end) = create_endpoints().unwrap();
        InteractionProtocolServerEnd::Test(server_end)
    }

    fn create_test_interaction_proxy_and_server_end(
    ) -> (TestInteractionProxy, InteractionProtocolServerEnd) {
        let (proxy, server_end) = create_proxy().unwrap();
        (proxy, InteractionProtocolServerEnd::Test(server_end))
    }

    fn create_password_interaction_protocol() -> InteractionProtocolServerEnd {
        let (_, server_end) = create_endpoints().unwrap();
        InteractionProtocolServerEnd::Password(server_end)
    }

    /// Starts the StorageUnlockMechanism server in the specified `mode`. Then
    /// it runs the `test_fn` checks its result. If `ok_server_result` is true,
    /// we verify that the server should return an `Ok` value. Otherwise it
    /// should return an error.
    async fn run_proxy_test<Fn, Fut>(mode: Mode, ok_server_result: bool, test_fn: Fn)
    where
        Fn: FnOnce(StorageUnlockMechanismProxy) -> Fut,
        Fut: Future<Output = Result<(), fidl::Error>>,
    {
        let (proxy, stream) = create_proxy_and_stream::<StorageUnlockMechanismMarker>().unwrap();
        let mechanism = StorageUnlockMechanism::new(mode);
        let server_fut = mechanism.handle_requests_from_stream(stream);
        let test_fut = test_fn(proxy);

        let (test_result, server_result) = join(test_fut, server_fut).await;
        assert!(test_result.is_ok());
        if ok_server_result {
            assert!(server_result.is_ok());
        } else {
            assert!(server_result.is_err());
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn always_succeed_enroll_and_authenticate_produce_same_prekey() {
        run_proxy_test(Mode::AlwaysSucceed, true, |proxy| async move {
            let (enrollment_data, enrollment_prekey) =
                proxy.enroll(&mut create_test_interaction_protocol()).await?.unwrap();

            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enrollment_data.clone() };

            let AttemptedEvent { enrollment_id, updated_enrollment_data, prekey_material, .. } =
                proxy
                    .authenticate(
                        &mut create_test_interaction_protocol(),
                        &mut vec![enrollment].iter_mut(),
                    )
                    .await?
                    .unwrap();

            assert_eq!(enrollment_id, Some(TEST_ENROLLMENT_ID));
            assert!(updated_enrollment_data.is_none());
            assert_eq!(prekey_material, Some(enrollment_prekey));
            Ok(())
        })
        .await
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn always_succeed_authenticate_multiple_enrollments() {
        run_proxy_test(Mode::AlwaysSucceed, true, |proxy| async move {
            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: vec![3] };
            let enrollment_2 = Enrollment { id: TEST_ENROLLMENT_ID_2, data: vec![12] };

            let AttemptedEvent { enrollment_id, updated_enrollment_data, prekey_material, .. } =
                proxy
                    .authenticate(
                        &mut create_test_interaction_protocol(),
                        &mut vec![enrollment, enrollment_2].iter_mut(),
                    )
                    .await?
                    .unwrap();

            assert_eq!(enrollment_id, Some(TEST_ENROLLMENT_ID));
            assert!(updated_enrollment_data.is_none());
            assert_eq!(prekey_material, Some(MAGIC_PREKEY.clone()));
            Ok(())
        })
        .await
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn always_fail_authentication_enroll_and_authenticate() {
        run_proxy_test(Mode::AlwaysFailAuthentication, true, |proxy| async move {
            let (enrollment_data, enrollment_prekey) =
                proxy.enroll(&mut create_test_interaction_protocol()).await?.unwrap();

            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enrollment_data.clone() };
            assert_eq!(enrollment_prekey, MAGIC_PREKEY.clone());

            let AttemptedEvent { enrollment_id, updated_enrollment_data, prekey_material, .. } =
                proxy
                    .authenticate(
                        &mut create_test_interaction_protocol(),
                        &mut vec![enrollment].iter_mut(),
                    )
                    .await?
                    .unwrap();

            assert_ne!(prekey_material, Some(MAGIC_PREKEY.clone()));
            assert!(updated_enrollment_data.is_none());
            assert_eq!(enrollment_id, Some(TEST_ENROLLMENT_ID));
            Ok(())
        })
        .await
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn interaction_no_set_success_authentication_enroll_and_authenticate() {
        run_proxy_test(Mode::Interaction, true, |proxy| async move {
            let (enrollment_data, enrollment_prekey) =
                proxy.enroll(&mut create_test_interaction_protocol()).await?.unwrap();

            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enrollment_data.clone() };
            assert_eq!(enrollment_prekey, MAGIC_PREKEY.clone());

            let (test_proxy, mut test_ipse) = create_test_interaction_proxy_and_server_end();
            std::mem::drop(test_proxy); // Drop test_proxy without calling SetSuccess().

            let auth_result =
                proxy.authenticate(&mut test_ipse, &mut vec![enrollment].iter_mut()).await?;

            // Should return "Aborted" since we closed the channel without
            // calling SetSuccess().
            assert_eq!(auth_result, Err(ApiError::Aborted));
            Ok(())
        })
        .await
    }

    #[fuchsia::test(allow_stalls = true)]
    async fn interaction_no_set_success_authentication_enroll_and_authenticate_timeout() {
        // Since we don't call SetSuccess on the TestInteraction channel, the
        // TestInteraction server handler, and hence the StorageUnlockMechanism
        // server handler will keep waiting and will not return.
        run_proxy_test(Mode::Interaction, false, |proxy| async move {
            let (enrollment_data, enrollment_prekey) =
                proxy.enroll(&mut create_test_interaction_protocol()).await?.unwrap();

            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enrollment_data.clone() };
            assert_eq!(enrollment_prekey, MAGIC_PREKEY.clone());

            let (_test_proxy, mut test_ipse) = create_test_interaction_proxy_and_server_end();

            let mut auth_fut = proxy
                .authenticate(&mut test_ipse, &mut vec![enrollment].iter_mut())
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
        run_proxy_test(Mode::Interaction, true, |proxy| async move {
            let (enrollment_data, enrollment_prekey) =
                proxy.enroll(&mut create_test_interaction_protocol()).await?.unwrap();

            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enrollment_data.clone() };
            let (test_proxy, mut test_ipse) = create_test_interaction_proxy_and_server_end();
            test_proxy.set_success()?;

            let AttemptedEvent { enrollment_id, updated_enrollment_data, prekey_material, .. } =
                proxy
                    .authenticate(&mut test_ipse, &mut vec![enrollment].iter_mut())
                    .await?
                    .unwrap();

            assert_eq!(enrollment_id, Some(TEST_ENROLLMENT_ID));
            assert!(updated_enrollment_data.is_none());
            assert_eq!(prekey_material, Some(enrollment_prekey));
            Ok(())
        })
        .await
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn interaction_set_success_and_authenticate_multiple_enrollments() {
        run_proxy_test(Mode::Interaction, true, |proxy| async move {
            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: vec![3] };
            let enrollment_2 = Enrollment { id: TEST_ENROLLMENT_ID_2, data: vec![12] };
            let (test_proxy, mut test_ipse) = create_test_interaction_proxy_and_server_end();
            test_proxy.set_success()?;

            let AttemptedEvent { enrollment_id, updated_enrollment_data, prekey_material, .. } =
                proxy
                    .authenticate(&mut test_ipse, &mut vec![enrollment, enrollment_2].iter_mut())
                    .await?
                    .unwrap();

            assert_eq!(enrollment_id, Some(TEST_ENROLLMENT_ID));
            assert!(updated_enrollment_data.is_none());
            assert_eq!(prekey_material, Some(MAGIC_PREKEY.clone()));
            Ok(())
        })
        .await
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn password_ipse_fail_enrollment_and_authentication() {
        run_proxy_test(Mode::AlwaysFailAuthentication, true, |proxy| async move {
            // Fail Enrollment since it only supports Test IPSE.
            assert_eq!(
                proxy.enroll(&mut create_password_interaction_protocol()).await?,
                Err(ApiError::InvalidRequest)
            );

            let (enrollment_data, enrollment_prekey) =
                proxy.enroll(&mut create_test_interaction_protocol()).await?.unwrap();

            let enrollment = Enrollment { id: TEST_ENROLLMENT_ID, data: enrollment_data.clone() };
            assert_eq!(enrollment_prekey, MAGIC_PREKEY.clone());

            // Fail authentication since it only supports Test IPSE.
            assert_eq!(
                proxy
                    .authenticate(
                        &mut create_password_interaction_protocol(),
                        &mut vec![enrollment].iter_mut(),
                    )
                    .await?,
                Err(ApiError::InvalidRequest)
            );

            Ok(())
        })
        .await
    }
}
