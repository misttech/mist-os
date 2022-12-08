// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;

use {
    crate::{
        account_metadata::AuthenticatorMetadata,
        keys::{
            EnrolledKey, Key, KeyEnrollment, KeyEnrollmentError, KeyRetrieval, KeyRetrievalError,
            KEY_LEN,
        },
        password_interaction::{ValidationError, Validator},
    },
    async_trait::async_trait,
    fuchsia_zircon as zx,
    identity_common::PrekeyMaterial,
    serde::{Deserialize, Serialize},
    thiserror::Error,
    tracing::error,
};

/// Parameters used with the scrypt key-derivation function.  These match the parameters
/// described in https://datatracker.ietf.org/doc/html/rfc7914
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScryptParams {
    salt: [u8; 16], // 16 byte random string
    log_n: u8,
    r: u32,
    p: u32,
}

#[derive(Error, Debug)]
pub enum ScryptError {
    #[error("Invalid scrypt params: {0}")]
    InvalidParams(#[source] scrypt::errors::InvalidParams),
    #[error("Output buffer too small: {0}")]
    BufferTooSmall(#[source] scrypt::errors::InvalidOutputLen),
}

impl ScryptParams {
    pub fn new() -> Self {
        // Generate a new random salt
        let mut salt = [0u8; 16];
        zx::cprng_draw(&mut salt);
        ScryptParams { salt, log_n: 15, r: 8, p: 1 }
    }

    /// Apply the scrypt key derivation function on the provided secret `bytes`, using the
    /// difficulty parameters and salt provided by `self`.
    pub fn scrypt(&self, bytes: &[u8]) -> Result<Key, ScryptError> {
        let params = scrypt::Params::new(self.log_n, self.r, self.p).map_err(|err| {
            error!("Invalid scrypt params: {}", err);
            ScryptError::InvalidParams(err)
        })?;
        let mut output = [0u8; KEY_LEN];
        scrypt::scrypt(bytes, &self.salt, &params, &mut output).map_err(|err| {
            error!("scrypt output buffer too small: {}", err);
            ScryptError::BufferTooSmall(err)
        })?;
        Ok(output)
    }
}

pub struct ScryptKeySource {
    scrypt_params: ScryptParams,
}

impl ScryptKeySource {
    pub fn new() -> ScryptKeySource {
        ScryptKeySource { scrypt_params: ScryptParams::new() }
    }
}

impl From<ScryptParams> for ScryptKeySource {
    fn from(item: ScryptParams) -> Self {
        ScryptKeySource { scrypt_params: item }
    }
}

#[async_trait]
impl KeyEnrollment<ScryptParams> for ScryptKeySource {
    async fn enroll_key(
        &mut self,
        password: &str,
    ) -> Result<EnrolledKey<ScryptParams>, KeyEnrollmentError> {
        let output = self
            .scrypt_params
            .scrypt(password.as_bytes())
            .map_err(|_| KeyEnrollmentError::ParamsError)?;
        Ok(EnrolledKey { key: output, enrollment_data: self.scrypt_params })
    }

    async fn remove_key(
        &mut self,
        _enrollment_data: ScryptParams,
    ) -> Result<(), KeyEnrollmentError> {
        Ok(())
    }
}

#[async_trait]
impl KeyRetrieval for ScryptKeySource {
    async fn retrieve_key(&self, password: &str) -> Result<Key, KeyRetrievalError> {
        let output = self
            .scrypt_params
            .scrypt(password.as_bytes())
            .map_err(|_| KeyRetrievalError::ParamsError)?;
        Ok(output)
    }
}

/// Implements the scrypt key derivation function for enrolling and
/// validating a password.
pub struct EnrollScryptValidator {}

#[async_trait]
impl Validator<(AuthenticatorMetadata, PrekeyMaterial)> for EnrollScryptValidator {
    async fn validate(
        &self,
        password: &str,
    ) -> Result<(AuthenticatorMetadata, PrekeyMaterial), ValidationError> {
        let mut key_source = ScryptKeySource::new();
        // TODO(fxb/115762): Add policy checks in a different crate/fn.
        key_source
            .enroll_key(password)
            .await
            .map(|enrolled_key| {
                (enrolled_key.enrollment_data.into(), PrekeyMaterial(enrolled_key.key.into()))
            })
            .map_err(|err| {
                ValidationError::InternalError(anyhow!("scrypt key enrollment failed: {:?}", err))
            })
    }
}

// A well-known set of (weak) params & salt, password, and corresponding key for tests, to avoid
// spending excessive CPU time doing expensive key derivations.
#[cfg(test)]
const TEST_SCRYPT_SALT: [u8; 16] =
    [202, 26, 165, 102, 212, 113, 114, 60, 106, 121, 183, 133, 36, 166, 127, 146];
#[cfg(test)]
pub const TEST_SCRYPT_PARAMS: ScryptParams =
    ScryptParams { salt: TEST_SCRYPT_SALT, log_n: 8, r: 8, p: 1 };

#[cfg(test)]
pub const TEST_SCRYPT_PASSWORD: &str = "test password";

// We have precomputed the key produced by the above fixed salt and params so that each test
// that wants to use one doesn't need to perform an additional key derivation every single time.
// A test below ensures that we verify our constant is correct.
#[cfg(test)]
pub const TEST_SCRYPT_KEY: [u8; KEY_LEN] = [
    88, 91, 129, 123, 173, 34, 21, 1, 23, 147, 87, 189, 56, 149, 89, 132, 210, 235, 150, 102, 129,
    93, 202, 53, 115, 170, 162, 217, 254, 115, 216, 181,
];

#[cfg(test)]
const FULL_STRENGTH_SCRYPT_SALT: [u8; 16] =
    [198, 228, 57, 32, 90, 251, 238, 12, 194, 62, 68, 106, 218, 187, 24, 246];
#[cfg(test)]
pub const FULL_STRENGTH_SCRYPT_PARAMS: ScryptParams =
    ScryptParams { salt: FULL_STRENGTH_SCRYPT_SALT, log_n: 15, r: 8, p: 1 };

#[cfg(test)]
mod test {
    use {super::*, anyhow::Result, assert_matches::assert_matches};

    #[fuchsia::test]
    async fn test_enroll_key() {
        let mut ks = ScryptKeySource::from(TEST_SCRYPT_PARAMS);
        let enrolled_key = ks.enroll_key(TEST_SCRYPT_PASSWORD).await.expect("enroll scrypt");
        assert_eq!(enrolled_key.key, TEST_SCRYPT_KEY);
        assert_matches!(enrolled_key.enrollment_data, TEST_SCRYPT_PARAMS);
    }

    #[fuchsia::test]
    async fn test_remove_key() {
        let mut ks = ScryptKeySource::from(TEST_SCRYPT_PARAMS);
        let enrolled_key = ks.enroll_key(TEST_SCRYPT_PASSWORD).await.expect("enroll scrypt");
        let res = ks.remove_key(enrolled_key.enrollment_data).await;
        assert_matches!(res, Ok(()));
    }

    #[fuchsia::test]
    async fn test_retrieve_key_weak_for_tests() {
        let ks = ScryptKeySource::from(TEST_SCRYPT_PARAMS);
        let key = ks.retrieve_key(TEST_SCRYPT_PASSWORD).await.expect("retrieve_key");
        assert_eq!(key, TEST_SCRYPT_KEY);
    }

    const GOLDEN_SCRYPT_PASSWORD: &str = "test password";
    const GOLDEN_SCRYPT_KEY: [u8; KEY_LEN] = [
        27, 250, 228, 96, 145, 67, 194, 114, 144, 240, 92, 150, 43, 136, 128, 51, 223, 120, 56,
        118, 124, 122, 106, 185, 159, 111, 178, 50, 86, 243, 227, 175,
    ];

    #[fuchsia::test]
    async fn test_retrieve_key_full_strength() {
        // Tests the full-strength key derivation against separately-verified constants.
        let ks = ScryptKeySource::from(FULL_STRENGTH_SCRYPT_PARAMS);
        let key = ks.retrieve_key(GOLDEN_SCRYPT_PASSWORD).await.expect("retrieve_key");
        assert_eq!(key, GOLDEN_SCRYPT_KEY);
    }

    #[fuchsia::test]
    async fn test_enrollment_validator() -> Result<()> {
        let validator = EnrollScryptValidator {};
        let result = validator.validate(TEST_SCRYPT_PASSWORD).await;
        assert!(result.is_ok());
        let (meta_data, prekey) = result.unwrap();
        assert_eq!(prekey.0.len(), KEY_LEN);

        let scrypt_meta = if let AuthenticatorMetadata::ScryptOnly(s) = meta_data {
            s.scrypt_params
        } else {
            return Err(anyhow!("wrong authenticator metadata type"));
        };

        assert_eq!(scrypt_meta.log_n, 15);
        assert_eq!(scrypt_meta.r, 8);
        assert_eq!(scrypt_meta.p, 1);

        Ok(())
    }
}
