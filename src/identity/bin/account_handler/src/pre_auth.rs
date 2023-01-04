// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This file contains a data type for pre-authentication state, which is
//! mutable per-account data that is readable when an account is locked.

use {
    crate::wrapped_key::{produce_wrapped_keys, WrappedKeySet},
    account_common::AccountId,
    aes_gcm::aead::generic_array::{typenum::U32, GenericArray},
    fidl_fuchsia_identity_account::Error as ApiError,
    fidl_fuchsia_identity_authentication::Mechanism,
    identity_common::{EnrollmentData, PrekeyMaterial},
    serde::{Deserialize, Deserializer, Serialize, Serializer},
    tracing::warn,
};

/// The current (latest) version of the Pre-auth state
const PRE_AUTH_STATE_VERSION: u32 = 1;

/// The pre-authentication state for an account.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct State {
    /// The version of the preauth state.
    ///
    /// Defining a version now lets us change fields over time (for example by deserializing into
    /// different structs based on the version) but currently we only support a single version
    /// and don't enforce any checks on that version.
    version: u32,

    /// The Account ID.
    account_id: AccountId,

    /// The enrollment state for this account.
    pub enrollment_state: EnrollmentState,
}

impl TryFrom<Vec<u8>> for State {
    type Error = ApiError;
    fn try_from(data: Vec<u8>) -> Result<Self, Self::Error> {
        bincode::deserialize(&data).map_err(|err| {
            warn!("Failed to deserialize Pre-auth state: {:?}", err);
            ApiError::InvalidRequest
        })
    }
}

impl<'a> TryInto<Vec<u8>> for &'a State {
    type Error = ApiError;

    fn try_into(self) -> Result<Vec<u8>, Self::Error> {
        bincode::serialize(self).map_err(|err| {
            warn!("Failed to serialize Pre-auth state: {:?}", err);
            ApiError::Internal
        })
    }
}

impl State {
    /// Constructs a new preauth state.
    pub fn new(account_id: AccountId, enrollment_state: EnrollmentState) -> Self {
        Self { version: PRE_AUTH_STATE_VERSION, account_id, enrollment_state }
    }

    /// Returns the account that this preauth state applies to.
    pub fn account_id(&self) -> &AccountId {
        &self.account_id
    }
}

/// State of the Enrollment associated with a system account.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum EnrollmentState {
    /// No authentication mechanism enrollments.
    NoEnrollments,

    /// A single enrollment of an authentication mechanism,
    /// containing:
    SingleEnrollment {
        #[serde(
            deserialize_with = "deserialize_mechanism",
            serialize_with = "serialize_mechanism"
        )]
        /// the mechanism used for authentication challenges.
        mechanism: Mechanism,
        /// the enrollment data for that authentication mechanism,
        data: EnrollmentData,
        /// both the volume encryption key and the null key (a key of all
        /// zeroes), wrapped with the authenticator prekey material.
        wrapped_key_material: WrappedKeySet,
    },
}

fn deserialize_mechanism<'de, D>(deserializer: D) -> Result<Mechanism, D::Error>
where
    D: Deserializer<'de>,
{
    let primitive = u32::deserialize(deserializer)?;
    Ok(Mechanism::from_primitive_allow_unknown(primitive))
}

fn serialize_mechanism<S>(mechanism: &Mechanism, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_u32(mechanism.into_primitive())
}

/// Produce an EnrollmentState::SingleEnrollment enum variant from {
/// |mechanism|, |enrollment_data| }. Use |prekey_material|
/// and |disk_key| to produce a wrapped disk encryption key, and also attach that to
/// the SingleEnrollment.
pub fn produce_single_enrollment(
    mechanism: Mechanism,
    enrollment_data: EnrollmentData,
    prekey_material: PrekeyMaterial,
    disk_key: &GenericArray<u8, U32>,
) -> Result<EnrollmentState, ApiError> {
    Ok(EnrollmentState::SingleEnrollment {
        mechanism,
        data: enrollment_data,
        wrapped_key_material: produce_wrapped_keys(prekey_material, disk_key)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrapped_key::WrappedKey;
    use fuchsia_async as fasync;
    use lazy_static::lazy_static;

    lazy_static! {
        static ref TEST_ENROLLMENT_STATE: EnrollmentState = EnrollmentState::SingleEnrollment {
            mechanism: Mechanism::Test,
            data: EnrollmentData(vec![1, 2, 3]),
            wrapped_key_material: WrappedKeySet {
                wrapped_disk_key: WrappedKey {
                    ciphertext_and_tag: vec![4, 5, 6],
                    nonce: [0_u8; 12]
                },
                wrapped_null_key: WrappedKey {
                    ciphertext_and_tag: vec![7, 8, 9],
                    nonce: [0_u8; 12]
                },
            }
        };
        static ref TEST_ACCOUNT_ID_1: AccountId = AccountId::new(1);
        static ref TEST_STATE: State =
            State::new(*TEST_ACCOUNT_ID_1, TEST_ENROLLMENT_STATE.clone(),);
        static ref TEST_STATE_BYTES: Vec<u8> = vec![
            1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 1,
            2, 3, 3, 0, 0, 0, 0, 0, 0, 0, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0,
            0, 0, 0, 0, 7, 8, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
        ];
    }

    #[fasync::run_until_stalled(test)]
    async fn convert_to_bytes_and_back() {
        let state_bytes: Vec<u8> = (&*TEST_STATE).try_into().unwrap();
        let state_from_bytes: State = state_bytes.try_into().unwrap();
        assert_eq!(&*TEST_STATE, &state_from_bytes);
    }

    #[fasync::run_until_stalled(test)]
    async fn convert_from_empty_bytes() {
        assert_eq!(State::try_from(vec![]), Err(ApiError::InvalidRequest));
    }

    #[fasync::run_until_stalled(test)]
    async fn check_golden() {
        let state_bytes: Vec<u8> = (&*TEST_STATE).try_into().unwrap();
        assert_eq!(state_bytes, *TEST_STATE_BYTES);
    }
}
