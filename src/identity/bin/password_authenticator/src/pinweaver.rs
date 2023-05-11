// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        account_metadata::AuthenticatorMetadata,
        keys::{
            EnrolledKey, Key, KeyEnrollment, KeyEnrollmentError, KeyRetrieval, KeyRetrievalError,
            KEY_LEN,
        },
        password_interaction::{ValidationError, Validator},
        policy,
        scrypt::{ScryptError, ScryptParams},
    },
    anyhow::anyhow,
    async_trait::async_trait,
    fidl_fuchsia_identity_credential::{self as fcred, ManagerMarker, ManagerProxy},
    fuchsia_component::client::connect_to_protocol,
    fuchsia_zircon as zx,
    futures::lock::Mutex,
    hmac::{Hmac, Mac, NewMac},
    identity_common::PrekeyMaterial,
    lazy_static::lazy_static,
    serde::{Deserialize, Serialize},
    sha2::Sha256,
    std::sync::Arc,
    tracing::{error, info},
};

#[cfg(test)]
use std::{collections::HashMap, sync::Mutex as SyncMutex};

// This file implements a key source which combines the computational hardness of scrypt with
// firmware-enforced rate-limiting of guesses.  The overall dataflow is:
//
//  -----------    HMAC-SHA256   -------------                              --------------
// |  password | (w/ fixed msg) | low-entropy |  rate-limited-by-firmware  | high-entropy |
// | from user | -------------> |    secret   | -------------------------> |    secret    |
//  -----------                  -------------                              --------------
//       |                                                                        |
//       | /----------------------------------------------------------------------/
//       | |
//    HMAC-SHA256
//       | |
//       V V
//  ------------                  ---------
// | mix secret |     scrypt     | account |
// |            | -------------> |   key   |
//  ------------                  ---------
//
// In this manner, we achieve the following properties:
//
// * account key cannot be computed without mix secret, and mix secret cannot be computed without
//   both password and high-entropy secret.
// * firmware does not gain direct knowledge of password, so even if firmware is compromised and
//   fails to secure high-entropy-secret, or if the low-entropy secret is pulled off the bus,
//   neither of these alone can compromise the account key

type HmacSha256 = Hmac<Sha256>;

type Label = u64;

/// Computes HMAC-SHA256(password, "password_authenticator") so that the low-entropy secret we
/// pass to credential_manager does not actually contain the user's passphrase.
fn compute_low_entropy_secret(password: &str) -> Key {
    // Create HMAC-SHA256 instance which implements `Mac` trait
    let mut mac =
        HmacSha256::new_from_slice(password.as_bytes()).expect("HMAC can take key of any size");
    mac.update(b"password_authenticator");

    // `result` has type `Output` which is a thin wrapper around array of
    // bytes for providing constant time equality check
    let result = mac.finalize();

    let mut key: Key = [0u8; KEY_LEN];
    key.clone_from_slice(&result.into_bytes());
    key
}

/// Computes mix_secret = HMAC-SHA256(password, high_entropy_secret).
/// This ensures that even if the secure element providing the high-entropy secret is compromised
/// in a way that leaks the key without the attacker needing to know the low-entropy secret, the
/// attacker still can't retrieve the account key without the user's password.
fn compute_mix_secret(password: &str, high_entropy_secret: &Key) -> Key {
    let mut mac =
        HmacSha256::new_from_slice(password.as_bytes()).expect("HMAC can take key of any size");
    mac.update(high_entropy_secret);
    let result = mac.finalize();
    let mut key: Key = [0u8; KEY_LEN];
    key.clone_from_slice(&result.into_bytes());
    key
}

/// Computes a key, given the mix_secret (which takes both the user's password and the
/// high-entropy secret as input) and a set of ScryptParams.  This ensures the computation still
/// requires some amount of CPU cost, memory cost, and is salted per-user.
fn compute_key(mix_secret: &Key, params: &ScryptParams) -> Result<Key, ScryptError> {
    params.scrypt(mix_secret)
}

/// A trait to support injecting credential manager implementations to enable
/// unit testing.
pub trait CredManagerProvider {
    type CM: CredManager;
    fn new_cred_manager(&self) -> Result<Self::CM, anyhow::Error>;
}

/// A CredManagerProvider that opens a new connection to the CredentialManager in our incoming
/// namespace whenever a CredManager instance is requested.
pub struct EnvCredManagerProvider {}

impl CredManagerProvider for EnvCredManagerProvider {
    type CM = ManagerProxy;

    fn new_cred_manager(&self) -> Result<Self::CM, anyhow::Error> {
        let proxy = connect_to_protocol::<ManagerMarker>().map_err(|err| {
            error!("unable to connect to credential manager from environment: {:?}", err);
            err
        })?;
        Ok(proxy)
    }
}

/// A trait which abstracts over the Credential Manager interface.
#[async_trait]
pub trait CredManager: Sync + Send {
    /// Enroll a (low-entropy, high-entropy) key pair with the credential manager, returning a
    /// credential label on success or a KeyEnrollmentError on failure.
    async fn add(&mut self, le_secret: &Key, he_secret: &Key) -> Result<Label, KeyEnrollmentError>;

    /// Retrieve the high-entropy secret associated with the provided low-entropy secret and
    /// credential label returned from a previous call to `add`.  Returns the high-entropy secret
    /// if the low-entropy secret and credential label match.
    async fn retrieve(&self, le_secret: &Key, cred_label: Label) -> Result<Key, KeyRetrievalError>;

    /// Remove any information associated with the credential label `cred_label` and release any
    /// resources.  Returns an error if removing the key pair failed, and unit on success.
    async fn remove(&mut self, cred_label: Label) -> Result<(), KeyEnrollmentError>;
}

lazy_static! {
    // use a delay_sched hardcoded into the password_authenticator binary, matching the
    // ChromeOS Pinweaver delay schedule for attempts 1-8, maxing out at 10 minutes between
    // attempts. Unlike the ChromeOS pinweaver implementation, we do not permanently lock out after
    // any number of attempts, since we are using this for the primary password, not purely for
    // a pin (with a password with no lockout as fallback).
    static ref DELAY_SCHEDULE: Vec<fcred::DelayScheduleEntry> = vec![
        fcred::DelayScheduleEntry {
            attempt_count: 5,
            time_delay: zx::Duration::from_seconds(20).into_nanos(),
        },
        fcred::DelayScheduleEntry {
            attempt_count: 6,
            time_delay: zx::Duration::from_seconds(60).into_nanos(),
        },
        fcred::DelayScheduleEntry {
            attempt_count: 7,
            time_delay: zx::Duration::from_seconds(300).into_nanos(),
        },
        fcred::DelayScheduleEntry {
            attempt_count: 8,
            time_delay: zx::Duration::from_seconds(600).into_nanos(),
        },
    ];
}

#[async_trait]
impl CredManager for ManagerProxy {
    /// Makes a request to the `CredentialManager` server represented by `self` to add the
    /// high-entropy secret `he_secret`, guarded by the low-entropy secret `le_secret`, with a
    /// delay schedule that rate-limits retrieval attempts.
    /// Returns the credential label given back to us by the `CredentialManager` instance on
    /// success. Propagates the FIDL error or the `fuchsia.identity.credential.CredentialError`
    /// given to us by `CredentialManager` on failure.
    async fn add(&mut self, le_secret: &Key, he_secret: &Key) -> Result<Label, KeyEnrollmentError> {
        // call AddCredential with the provided parameters and the hardcoded delay_schedule
        let params = fcred::AddCredentialParams {
            le_secret: Some(le_secret.to_vec()),
            delay_schedule: Some(DELAY_SCHEDULE.to_vec()),
            he_secret: Some(he_secret.to_vec()),
            ..Default::default()
        };

        self.add_credential(&params)
            .await
            .map_err(|err| {
                error!("CredentialManager#AddCredential: couldn't send FIDL request: {:?}", err);
                KeyEnrollmentError::FidlError(err)
            })?
            .map_err(|err| {
                error!("CredentialManager#AddCredential: couldn't add credential: {:?}", err);
                KeyEnrollmentError::CredentialManagerError(err)
            })
    }

    /// Makes a request to the CredentialManager server represented by self to retrieve the
    /// high-entropy secret for the credential identified by `cred_label` by providing `le_secret`.
    /// If caller provided the same `le_secret` used in the `add()` call that returned the
    /// `cred_label` they provided, we should receive the high-entropy secret back and return Ok.
    async fn retrieve(&self, le_secret: &Key, cred_label: Label) -> Result<Key, KeyRetrievalError> {
        let params = fcred::CheckCredentialParams {
            le_secret: Some(le_secret.to_vec()),
            label: Some(cred_label),
            ..Default::default()
        };

        self.check_credential(&params)
            .await
            .map_err(|err| {
                error!("CredentialManager#CheckCredential: couldn't send FIDL request: {:?}", err);
                KeyRetrievalError::FidlError(err)
            })?
            .map_err(|err| {
                info!(
                    "CredentialManager#CheckCredential: credential manager returned error {:?}",
                    err
                );
                KeyRetrievalError::CredentialManagerError(err)
            })
            .map(|resp| resp.he_secret)?
            .ok_or_else(|| {
                error!(
                    "CredentialManager#CheckCredential: credential manager returned success, \
                       but did not provide an `he_secret` value"
                );
                KeyRetrievalError::InvalidCredentialManagerDataError
            })
            .map(|key| {
                let mut res: [u8; KEY_LEN] = [0; KEY_LEN];
                if key.len() == KEY_LEN {
                    res.clone_from_slice(&key[..]);
                    Ok(res)
                } else {
                    error!(
                        expected_bytes = KEY_LEN,
                        got_bytes = key.len(),
                        "CredentialManager#CheckCredential: credential manager returned key \
                           with invalid length"
                    );
                    Err(KeyRetrievalError::InvalidCredentialManagerDataError)
                }
            })?
    }

    /// Makes a request to the CredentialManager server represented by self to remove the
    /// credential identified by `cred_label`, releasing any storage associated with it and
    /// making it invalid to use with future `retrieve` calls (unless returned as the result of a
    /// new call to `add`).  On failure, propagates the error from the FIDL call.
    async fn remove(&mut self, cred_label: Label) -> Result<(), KeyEnrollmentError> {
        self.remove_credential(cred_label)
            .await
            .map_err(|err| {
                error!("CredentialManager#RemoveCredential: couldn't send FIDL request: {:?}", err);
                KeyEnrollmentError::FidlError(err)
            })?
            .map_err(|err| {
                error!("CredentialManager#RemoveCredential: couldn't remove credential: {:?}", err);
                KeyEnrollmentError::CredentialManagerError(err)
            })
    }
}

/// Key enrollment data for the pinweaver key source.  Provides sufficient context to retrieve a
/// key after enrollment.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PinweaverParams {
    /// The scrypt difficulty parameters and salt to be used in the final key derivation step.
    pub scrypt_params: ScryptParams,

    /// The credential label returned from the credential manager backing store, which uniquely
    /// identifies the enrolled credential, so we can attempt to retrieve the high-entropy secret.
    pub credential_label: Label,
}

/// An implementation of KeyEnrollment using the pinweaver dataflow, backed by some CredManager
/// implementation.
pub struct PinweaverKeyEnroller<CM>
where
    CM: CredManager,
{
    scrypt_params: ScryptParams,
    credential_manager: Arc<Mutex<CM>>,
}

impl<CM> PinweaverKeyEnroller<CM>
where
    CM: CredManager,
{
    pub fn new(credential_manager: Arc<Mutex<CM>>) -> PinweaverKeyEnroller<CM> {
        PinweaverKeyEnroller { scrypt_params: ScryptParams::new(), credential_manager }
    }
}

#[async_trait]
impl<CM> KeyEnrollment<PinweaverParams> for PinweaverKeyEnroller<CM>
where
    CM: CredManager,
{
    async fn enroll_key(
        &mut self,
        password: &str,
    ) -> Result<EnrolledKey<PinweaverParams>, KeyEnrollmentError> {
        // compute a 256-bit le_secret as HMAC-SHA256(password, “password_authenticator”);
        let low_entropy_secret = compute_low_entropy_secret(password);

        // generate a 256-bit random he_secret
        let mut high_entropy_secret: [u8; KEY_LEN] = [0; KEY_LEN];
        zx::cprng_draw(&mut high_entropy_secret);

        // Enroll the secret with the credential manager
        let label = self
            .credential_manager
            .lock()
            .await
            .add(&low_entropy_secret, &high_entropy_secret)
            .await?;

        // compute mix_secret = HMAC-SHA256(password, he_secret)
        let mix_secret = compute_mix_secret(password, &high_entropy_secret);

        // compute key = scrypt(mix_secret, salt, N, p, r)
        let key: Key = compute_key(&mix_secret, &self.scrypt_params)
            .map_err(|_| KeyEnrollmentError::ParamsError)?;

        Ok(EnrolledKey::<PinweaverParams> {
            key,
            enrollment_data: PinweaverParams {
                scrypt_params: self.scrypt_params,
                credential_label: label,
            },
        })
    }

    async fn remove_key(
        &mut self,
        enrollment_data: PinweaverParams,
    ) -> Result<(), KeyEnrollmentError> {
        // Tell the credential manager to remove the credential identified by the credential label
        let mut cm = self.credential_manager.lock().await;
        cm.remove(enrollment_data.credential_label).await
    }
}

pub struct PinweaverKeyRetriever<CM>
where
    CM: CredManager,
{
    pinweaver_params: PinweaverParams,
    credential_manager: Arc<Mutex<CM>>,
}

impl<CM> PinweaverKeyRetriever<CM>
where
    CM: CredManager,
{
    pub fn new(
        pinweaver_params: PinweaverParams,
        credential_manager: Arc<Mutex<CM>>,
    ) -> PinweaverKeyRetriever<CM> {
        PinweaverKeyRetriever { pinweaver_params, credential_manager }
    }
}

#[async_trait]
impl<CM> KeyRetrieval for PinweaverKeyRetriever<CM>
where
    CM: CredManager,
{
    async fn retrieve_key(&self, password: &str) -> Result<Key, KeyRetrievalError> {
        let low_entropy_secret = compute_low_entropy_secret(password);
        let high_entropy_secret = self
            .credential_manager
            .lock()
            .await
            .retrieve(&low_entropy_secret, self.pinweaver_params.credential_label)
            .await?;

        // compute mix_secret = HMAC-SHA256(password, he_secret)
        let mix_secret = compute_mix_secret(password, &high_entropy_secret);

        // compute key = scrypt(mix_secret, salt, N, p, r)
        let key: Key = compute_key(&mix_secret, &self.pinweaver_params.scrypt_params)
            .map_err(|_| KeyRetrievalError::ParamsError)?;

        Ok(key)
    }
}

/// Implements the pinweaver key derivation function for enrolling and
/// validating a password.
pub struct EnrollPinweaverValidator<CM>
where
    CM: CredManager,
{
    // Wrap CM in an Arc<Mutex<>> because it needs to be owned by
    // PinweaverKeyEnroller.
    cred_manager: Arc<Mutex<CM>>,
}

impl<CM> EnrollPinweaverValidator<CM>
where
    CM: CredManager,
{
    /// Instantiate an EnrollPinweaverValidator with a given CM.
    pub fn new(cred_manager: CM) -> Self {
        Self { cred_manager: Arc::new(Mutex::new(cred_manager)) }
    }
}

#[async_trait]
impl<CM> Validator<(AuthenticatorMetadata, PrekeyMaterial)> for EnrollPinweaverValidator<CM>
where
    CM: CredManager + std::marker::Sync + std::marker::Send,
{
    async fn validate(
        &self,
        password: &str,
    ) -> Result<(AuthenticatorMetadata, PrekeyMaterial), ValidationError> {
        policy::check(password).map_err(ValidationError::PasswordError)?;
        let mut key_source = PinweaverKeyEnroller::new(Arc::clone(&self.cred_manager));
        key_source
            .enroll_key(password)
            .await
            .map(|enrolled_key| {
                (enrolled_key.enrollment_data.into(), PrekeyMaterial(enrolled_key.key.into()))
            })
            .map_err(|err| {
                ValidationError::DependencyError(anyhow!(
                    "pinweaver key enrollment failed: {:?}",
                    err
                ))
            })
    }
}

/// Implements the pinweaver key retrieval function for authenticating and
/// validating a password.
pub struct AuthenticatePinweaverValidator<CM>
where
    CM: CredManager,
{
    // Wrap CM in an Arc<Mutex<>> because it needs to be owned by
    // PinweaverKeyRetriever.
    cred_manager: Arc<Mutex<CM>>,
    pinweaver_params: PinweaverParams,
}

impl<CM> AuthenticatePinweaverValidator<CM>
where
    CM: CredManager,
{
    /// Instantiate an AuthenticatePinweaverValidator with given CM and PinweaverParams.
    pub fn new(cred_manager: CM, pinweaver_params: PinweaverParams) -> Self {
        Self { cred_manager: Arc::new(Mutex::new(cred_manager)), pinweaver_params }
    }
}

#[async_trait]
impl<CM> Validator<PrekeyMaterial> for AuthenticatePinweaverValidator<CM>
where
    CM: CredManager + std::marker::Sync + std::marker::Send,
{
    async fn validate(&self, password: &str) -> Result<PrekeyMaterial, ValidationError> {
        let key_source =
            PinweaverKeyRetriever::new(self.pinweaver_params, Arc::clone(&self.cred_manager));
        key_source.retrieve_key(password).await.map(|key| PrekeyMaterial(key.into())).map_err(
            |err| match err {
                KeyRetrievalError::PasswordError => {
                    ValidationError::InternalError(anyhow!("Password did not meet preconditions."))
                }
                KeyRetrievalError::ParamsError => {
                    ValidationError::InternalError(anyhow!("Invalid parameters provided."))
                }
                KeyRetrievalError::FidlError(e) => {
                    ValidationError::DependencyError(anyhow!("Dependency error: {:?}", e))
                }
                KeyRetrievalError::CredentialManagerConnectionError(e) => {
                    ValidationError::DependencyError(anyhow!("Dependency error: {:?}", e))
                }
                KeyRetrievalError::CredentialManagerError(e) => {
                    ValidationError::DependencyError(anyhow!("Dependency error: {:?}", e))
                }
                KeyRetrievalError::InvalidCredentialManagerDataError => {
                    ValidationError::DependencyError(anyhow!(
                        "Credential manager returned invalid data"
                    ))
                }
            },
        )
    }
}

#[cfg(test)]
#[derive(Clone, Debug)]
enum EnrollBehavior {
    AsExpected,
    ExpectLeSecret(Key),
    ForceFailWithEnrollmentError,
}

#[cfg(test)]
#[derive(Clone, Debug)]
enum RetrieveBehavior {
    AsExpected,
    ForceFailWithRetrievalError,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub struct MockCredManagerProvider {
    mcm: MockCredManager,
}

#[cfg(test)]
impl MockCredManagerProvider {
    pub fn new() -> MockCredManagerProvider {
        MockCredManagerProvider { mcm: MockCredManager::new() }
    }

    pub fn new_with_cred_manager(mcm: MockCredManager) -> MockCredManagerProvider {
        MockCredManagerProvider { mcm }
    }
}

#[cfg(test)]
impl CredManagerProvider for MockCredManagerProvider {
    type CM = MockCredManager;
    fn new_cred_manager(&self) -> Result<Self::CM, anyhow::Error> {
        Ok(self.mcm.clone())
    }
}

/// An in-memory mock of a CredentialManager implementation which stores all secrets in a
/// HashMap in memory.
#[cfg(test)]
#[derive(Clone, Debug)]
pub struct MockCredManager {
    creds: Arc<SyncMutex<HashMap<Label, (Key, Key)>>>,
    enroll_behavior: EnrollBehavior,
    retrieve_behavior: RetrieveBehavior,
}

#[cfg(test)]
impl MockCredManager {
    pub fn new_with_creds(creds: Arc<SyncMutex<HashMap<Label, (Key, Key)>>>) -> MockCredManager {
        MockCredManager {
            creds,
            enroll_behavior: EnrollBehavior::AsExpected,
            retrieve_behavior: RetrieveBehavior::AsExpected,
        }
    }

    pub fn new() -> MockCredManager {
        MockCredManager::new_with_creds(Arc::new(SyncMutex::new(HashMap::new())))
    }

    pub fn new_expect_le_secret(le_secret: &Key) -> MockCredManager {
        MockCredManager {
            creds: Arc::new(SyncMutex::new(HashMap::new())),
            enroll_behavior: EnrollBehavior::ExpectLeSecret(*le_secret),
            retrieve_behavior: RetrieveBehavior::AsExpected,
        }
    }

    pub fn new_fail_enrollment() -> MockCredManager {
        MockCredManager {
            creds: Arc::new(SyncMutex::new(HashMap::new())),
            enroll_behavior: EnrollBehavior::ForceFailWithEnrollmentError,
            retrieve_behavior: RetrieveBehavior::AsExpected,
        }
    }

    fn new_fail_retrieval() -> MockCredManager {
        MockCredManager {
            creds: Arc::new(SyncMutex::new(HashMap::new())),
            enroll_behavior: EnrollBehavior::AsExpected,
            retrieve_behavior: RetrieveBehavior::ForceFailWithRetrievalError,
        }
    }

    pub fn insert_with_next_free_label(
        &mut self,
        le_secret: Key,
        he_secret: Key,
    ) -> (Label, Option<(Key, Key)>) {
        let mut creds = self.creds.lock().unwrap();
        // Select the next label not in use.
        let mut label = 1;
        while creds.contains_key(&label) {
            label += 1;
        }
        let prev_value = creds.insert(label, (le_secret, he_secret));
        (label, prev_value)
    }

    pub fn get_number_of_creds(&self) -> usize {
        self.creds.lock().expect("cannot unlock").len()
    }
}

#[cfg(test)]
#[async_trait]
impl CredManager for MockCredManager {
    async fn add(&mut self, le_secret: &Key, he_secret: &Key) -> Result<Label, KeyEnrollmentError> {
        match &self.enroll_behavior {
            EnrollBehavior::AsExpected => {
                let (label, _) = self.insert_with_next_free_label(*le_secret, *he_secret);
                Ok(label)
            }
            EnrollBehavior::ExpectLeSecret(key) => {
                assert_eq!(key, le_secret);
                let (label, _) = self.insert_with_next_free_label(*le_secret, *he_secret);
                Ok(label)
            }
            EnrollBehavior::ForceFailWithEnrollmentError => {
                Err(KeyEnrollmentError::CredentialManagerError(fcred::CredentialError::NoFreeLabel))
            }
        }
    }

    async fn retrieve(&self, le_secret: &Key, cred_label: Label) -> Result<Key, KeyRetrievalError> {
        match self.retrieve_behavior {
            RetrieveBehavior::AsExpected => {
                let creds = self.creds.lock().unwrap();
                match creds.get(&cred_label) {
                    Some((low_entropy_secret, high_entropy_secret)) => {
                        if le_secret == low_entropy_secret {
                            Ok(*high_entropy_secret)
                        } else {
                            Err(KeyRetrievalError::CredentialManagerError(
                                fcred::CredentialError::InvalidSecret,
                            ))
                        }
                    }
                    None => Err(KeyRetrievalError::CredentialManagerError(
                        fcred::CredentialError::InvalidLabel,
                    )),
                }
            }
            RetrieveBehavior::ForceFailWithRetrievalError => Err(
                KeyRetrievalError::CredentialManagerError(fcred::CredentialError::InvalidSecret),
            ),
        }
    }

    async fn remove(&mut self, cred_label: Label) -> Result<(), KeyEnrollmentError> {
        let mut creds = self.creds.lock().unwrap();
        let prev = creds.remove(&cred_label);
        match prev {
            Some(_) => Ok(()),
            None => Err(KeyEnrollmentError::CredentialManagerError(
                fcred::CredentialError::InvalidLabel,
            )),
        }
    }
}

#[cfg(test)]
pub const TEST_PINWEAVER_CREDENTIAL_LABEL: Label = 1u64;

/// The fixed low-entropy secret derived from TEST_SCRYPT_PASSWORD
#[cfg(test)]
pub const TEST_PINWEAVER_LE_SECRET: [u8; KEY_LEN] = [
    230, 21, 65, 10, 158, 243, 134, 222, 213, 187, 110, 176, 44, 67, 246, 104, 137, 26, 30, 76, 90,
    12, 229, 169, 241, 31, 123, 127, 178, 76, 210, 210,
];
/// A fixed key generated and recorded in source for test usage to enable deterministic testing
/// of key-derivation computations.  In practice, high-entropy secrets will be randomly
/// generated at runtime.
#[cfg(test)]
pub const TEST_PINWEAVER_HE_SECRET: [u8; KEY_LEN] = [
    165, 169, 79, 36, 201, 215, 227, 13, 74, 62, 115, 217, 71, 229, 180, 70, 233, 76, 139, 10, 55,
    49, 182, 163, 113, 209, 83, 18, 248, 250, 189, 153,
];
/// Derived from TEST_PINWEAVER_HE_SECRET and TEST_SCRYPT_PASSWORD
#[cfg(test)]
pub const TEST_PINWEAVER_MIX_SECRET: [u8; KEY_LEN] = [
    94, 201, 56, 224, 222, 39, 239, 116, 198, 113, 209, 14, 37, 140, 225, 6, 144, 168, 246, 212,
    239, 145, 233, 119, 229, 0, 91, 138, 142, 22, 10, 195,
];
/// Derived from TEST_PINWEAVER_MIX_SECRET and TEST_SCRYPT_PARAMS
#[cfg(test)]
pub const TEST_PINWEAVER_KEY: [u8; KEY_LEN] = [
    228, 50, 47, 112, 78, 137, 56, 116, 50, 180, 30, 230, 55, 132, 33, 117, 119, 187, 221, 250, 73,
    193, 216, 194, 37, 177, 70, 45, 209, 216, 49, 110,
];

#[cfg(test)]
mod test {
    use {
        super::*,
        crate::scrypt::{TEST_SCRYPT_PARAMS, TEST_SCRYPT_PASSWORD},
        anyhow::Result,
        assert_matches::assert_matches,
    };

    #[fuchsia::test]
    async fn test_enroll_key() {
        let expected_le_secret = compute_low_entropy_secret(TEST_SCRYPT_PASSWORD);
        let mcm = MockCredManager::new_expect_le_secret(&expected_le_secret);
        let mut pw_enroller = PinweaverKeyEnroller::new(Arc::new(Mutex::new(mcm)));
        let enrolled_key =
            pw_enroller.enroll_key(TEST_SCRYPT_PASSWORD).await.expect("enrollment should succeed");
        assert_matches!(
            enrolled_key.enrollment_data,
            PinweaverParams { scrypt_params: _, credential_label: TEST_PINWEAVER_CREDENTIAL_LABEL }
        );
    }

    #[fuchsia::test]
    async fn test_enroll_key_fail() {
        let mcm = MockCredManager::new_fail_enrollment();
        let mut pw_enroller = PinweaverKeyEnroller::new(Arc::new(Mutex::new(mcm)));
        let err =
            pw_enroller.enroll_key(TEST_SCRYPT_PASSWORD).await.expect_err("enrollment should fail");
        assert_matches!(
            err,
            KeyEnrollmentError::CredentialManagerError(fcred::CredentialError::NoFreeLabel)
        );
    }

    #[fuchsia::test]
    async fn test_pinweaver_goldens() {
        let low_entropy_secret = compute_low_entropy_secret(TEST_SCRYPT_PASSWORD);
        assert_eq!(low_entropy_secret, TEST_PINWEAVER_LE_SECRET);

        let mix_secret = compute_mix_secret(TEST_SCRYPT_PASSWORD, &TEST_PINWEAVER_HE_SECRET);
        assert_eq!(mix_secret, TEST_PINWEAVER_MIX_SECRET);

        let key = compute_key(&mix_secret, &TEST_SCRYPT_PARAMS).expect("compute account key");
        assert_eq!(key, TEST_PINWEAVER_KEY);
    }

    #[fuchsia::test]
    async fn test_retrieve_key() {
        let mut mcm = MockCredManager::new();
        let le_secret = compute_low_entropy_secret(TEST_SCRYPT_PASSWORD);
        let label =
            mcm.add(&le_secret, &TEST_PINWEAVER_HE_SECRET).await.expect("enroll key with mock");
        let pw_retriever = PinweaverKeyRetriever::new(
            PinweaverParams { scrypt_params: TEST_SCRYPT_PARAMS, credential_label: label },
            Arc::new(Mutex::new(mcm)),
        );
        let key_retrieved =
            pw_retriever.retrieve_key(TEST_SCRYPT_PASSWORD).await.expect("key should be found");
        assert_eq!(key_retrieved, TEST_PINWEAVER_KEY);
    }

    #[fuchsia::test]
    async fn test_retrieve_key_fail() {
        let mcm = MockCredManager::new_fail_retrieval();
        let pw_retriever = PinweaverKeyRetriever::new(
            PinweaverParams {
                scrypt_params: TEST_SCRYPT_PARAMS,
                credential_label: TEST_PINWEAVER_CREDENTIAL_LABEL,
            },
            Arc::new(Mutex::new(mcm)),
        );
        let err = pw_retriever
            .retrieve_key(TEST_SCRYPT_PASSWORD)
            .await
            .expect_err("retrieval should fail");
        assert_matches!(
            err,
            KeyRetrievalError::CredentialManagerError(fcred::CredentialError::InvalidSecret)
        );
    }

    #[fuchsia::test]
    async fn test_remove_key() {
        let creds = Arc::new(SyncMutex::new(HashMap::new()));
        let mcm = MockCredManager::new_with_creds(creds.clone());
        let mut pw_enroller = PinweaverKeyEnroller::new(Arc::new(Mutex::new(mcm)));
        let enrolled_key = pw_enroller.enroll_key(TEST_SCRYPT_PASSWORD).await.expect("enroll");
        let remove_res = pw_enroller.remove_key(enrolled_key.enrollment_data).await;
        assert_matches!(remove_res, Ok(()));
    }

    #[fuchsia::test]
    async fn test_roundtrip() {
        // Enroll a key and get the enrollment data.
        let creds = Arc::new(SyncMutex::new(HashMap::new()));
        let mcm = MockCredManager::new_with_creds(creds.clone());
        let mut pw_enroller = PinweaverKeyEnroller::new(Arc::new(Mutex::new(mcm)));
        let enrolled_key = pw_enroller.enroll_key(TEST_SCRYPT_PASSWORD).await.expect("enroll");
        let key = enrolled_key.key;
        let enrollment_data = enrolled_key.enrollment_data;

        // Retrieve the key, and verify it matches.
        let mcm2 = MockCredManager::new_with_creds(creds.clone());
        let pw_retriever = PinweaverKeyRetriever::new(enrollment_data, Arc::new(Mutex::new(mcm2)));
        let key_retrieved =
            pw_retriever.retrieve_key(TEST_SCRYPT_PASSWORD).await.expect("retrieve");
        assert_eq!(key_retrieved, key);

        // Remove the key.
        let remove_res = pw_enroller.remove_key(enrollment_data).await;
        assert_matches!(remove_res, Ok(()));

        // Retrieving the key again should fail.
        let retrieve_res = pw_retriever.retrieve_key(TEST_SCRYPT_PASSWORD).await;
        assert_matches!(
            retrieve_res,
            Err(KeyRetrievalError::CredentialManagerError(fcred::CredentialError::InvalidLabel))
        );

        // Removing the key again should fail.
        let remove_res2 = pw_enroller.remove_key(enrollment_data).await;
        assert_matches!(
            remove_res2,
            Err(KeyEnrollmentError::CredentialManagerError(fcred::CredentialError::InvalidLabel))
        );
    }

    #[fuchsia::test]
    async fn test_enrollment_validator_success() -> Result<()> {
        let expected_le_secret = compute_low_entropy_secret(TEST_SCRYPT_PASSWORD);
        let mcm = MockCredManager::new_expect_le_secret(&expected_le_secret);

        let validator = EnrollPinweaverValidator::new(mcm);
        let result = validator.validate(TEST_SCRYPT_PASSWORD).await;

        assert!(result.is_ok());

        let (meta_data, prekey) = result.unwrap();

        let pinweaver_data = if let AuthenticatorMetadata::Pinweaver(p) = meta_data {
            p.pinweaver_params
        } else {
            return Err(anyhow!("wrong authenticator metadata type"));
        };

        assert_matches!(
            pinweaver_data,
            PinweaverParams { scrypt_params: _, credential_label: TEST_PINWEAVER_CREDENTIAL_LABEL }
        );

        assert_eq!(prekey.0.len(), KEY_LEN);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_enrollment_password_too_short() {
        let validator = EnrollPinweaverValidator::new(MockCredManager::new());
        let result = validator.validate("short").await;
        assert_matches!(result, Err(ValidationError::PasswordError(_)));
    }

    #[fuchsia::test]
    async fn test_enrollment_validator_failure() {
        let mcm = MockCredManager::new_fail_enrollment();

        let validator = EnrollPinweaverValidator::new(mcm);
        let result = validator.validate(TEST_SCRYPT_PASSWORD).await;

        assert_matches!(result, Err(ValidationError::DependencyError(_)));
    }

    #[fuchsia::test]
    async fn test_authenticator_validator_failure() {
        let mcm = MockCredManager::new();

        let validator = AuthenticatePinweaverValidator::new(
            mcm,
            PinweaverParams { scrypt_params: TEST_SCRYPT_PARAMS, credential_label: 0 },
        );
        let result = validator.validate(TEST_SCRYPT_PASSWORD).await;
        assert_matches!(result, Err(ValidationError::DependencyError(_)));
    }

    #[fuchsia::test]
    async fn test_authenticator_validator_success() {
        let mut mcm = MockCredManager::new();
        let le_secret = compute_low_entropy_secret(TEST_SCRYPT_PASSWORD);
        let label =
            mcm.add(&le_secret, &TEST_PINWEAVER_HE_SECRET).await.expect("enroll key with mock");
        let validator = AuthenticatePinweaverValidator::new(
            mcm,
            PinweaverParams { scrypt_params: TEST_SCRYPT_PARAMS, credential_label: label },
        );

        let result = validator.validate(TEST_SCRYPT_PASSWORD).await;
        assert!(result.is_ok());

        let prekey = result.unwrap();
        assert_eq!(prekey.0.len(), KEY_LEN);
    }

    #[fuchsia::test]
    async fn test_validator_roundtrip() -> Result<()> {
        let expected_le_secret = compute_low_entropy_secret(TEST_SCRYPT_PASSWORD);
        let mcm = MockCredManager::new_expect_le_secret(&expected_le_secret);

        let enroll_validator = EnrollPinweaverValidator::new(mcm.clone());
        let (meta_data, enroll_prekey) =
            enroll_validator.validate(TEST_SCRYPT_PASSWORD).await.unwrap();

        let pinweaver_data = if let AuthenticatorMetadata::Pinweaver(p) = meta_data {
            p.pinweaver_params
        } else {
            return Err(anyhow!("wrong authenticator metadata type"));
        };

        assert_matches!(
            pinweaver_data,
            PinweaverParams { scrypt_params: _, credential_label: TEST_PINWEAVER_CREDENTIAL_LABEL }
        );

        assert_eq!(enroll_prekey.0.len(), KEY_LEN);

        let auth_validator = AuthenticatePinweaverValidator::new(mcm, pinweaver_data);
        let auth_prekey = auth_validator.validate(TEST_SCRYPT_PASSWORD).await.unwrap();

        assert_eq!(enroll_prekey, auth_prekey);

        Ok(())
    }
}
