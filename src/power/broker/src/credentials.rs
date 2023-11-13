// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fidl_fuchsia_power_broker::{self as fpb, Permissions};
use fuchsia_zircon::{self as zx, AsHandleRef};
use std::collections::HashMap;

use crate::topology::ElementID;

/// Use kernel object IDs to uniquely represent Credentials. They are
/// guaranteed not to be reused over the lifetime of the system:
/// https://fuchsia.dev/fuchsia-src/concepts/kernel/concepts#kernel_object_ids
type CredentialID = zx::Koid;

/// A Credential that has been created and registered with the system.
#[derive(Clone, Debug, PartialEq)]
pub struct Credential {
    id: CredentialID,
    element: ElementID,
    permissions: Permissions,
}

impl Credential {
    pub fn get_element(&self) -> &ElementID {
        &self.element
    }

    pub fn contains(&self, permissions: Permissions) -> bool {
        self.permissions.contains(permissions)
    }

    /// Returns true iff self includes these permissions for the given element.
    pub fn authorizes(&self, element: &ElementID, permissions: &Permissions) -> bool {
        self.element == *element && self.permissions.contains(*permissions)
    }
}

#[derive(Debug)]
pub struct Token {
    event_pair: zx::EventPair,
}

impl From<zx::EventPair> for Token {
    fn from(event_pair: zx::EventPair) -> Self {
        Token { event_pair }
    }
}

impl Token {
    fn koid(&self) -> Option<zx::Koid> {
        let Ok(info) = self.event_pair.basic_info() else {
            return None;
        };
        Some(info.koid)
    }

    fn related_koid(&self) -> Option<zx::Koid> {
        let Ok(info) = self.event_pair.basic_info() else {
            return None;
        };
        Some(info.related_koid)
    }
}

/// CredentialToRegister holds the necessary information for a Credential
/// to be created and registered. It is meant for clients to use to specify
/// new Credentials to be registered.
#[derive(Debug)]
pub struct CredentialToRegister {
    pub broker_token: Token,
    pub permissions: Permissions,
}

impl From<fpb::Credential> for CredentialToRegister {
    fn from(c: fpb::Credential) -> Self {
        Self { broker_token: c.broker_token.into(), permissions: c.permissions.into() }
    }
}

#[derive(Debug)]
pub struct Registry {
    credentials: HashMap<CredentialID, Credential>,
    tokens: HashMap<CredentialID, Token>,
    credential_ids_by_element: HashMap<ElementID, Vec<CredentialID>>,
}

impl Registry {
    pub fn new() -> Self {
        Registry {
            credentials: HashMap::new(),
            tokens: HashMap::new(),
            credential_ids_by_element: HashMap::new(),
        }
    }

    pub fn get_credential_by_koid(&self, koid: &zx::Koid) -> Option<Credential> {
        self.credentials.get(koid).cloned()
    }

    pub fn lookup(&self, token: Token) -> Option<Credential> {
        let Some(related_koid) = token.related_koid() else {
            tracing::debug!("could not get related koid for {:?}", token);
            return None;
        };
        self.get_credential_by_koid(&related_koid)
    }

    pub fn register(
        &mut self,
        element: &ElementID,
        credential_to_register: CredentialToRegister,
    ) -> Result<CredentialID, RegisterCredentialsError> {
        let Some(id) = credential_to_register.broker_token.koid() else {
            tracing::error!("could not get koid for {:?}", credential_to_register.broker_token);
            return Err(RegisterCredentialsError::Internal);
        };
        self.tokens.insert(id, credential_to_register.broker_token);
        self.credential_ids_by_element.entry(element.clone()).or_insert(Vec::new()).push(id);
        let credential = Credential {
            id: id,
            element: element.clone(),
            permissions: credential_to_register.permissions,
        };
        tracing::debug!("registered credential: {:?}", &credential);
        self.credentials.insert(id, credential);
        Ok(id)
    }

    pub fn unregister(&mut self, credential: &Credential) -> Option<Credential> {
        self.unregister_id(&credential.id)
    }

    fn unregister_id(&mut self, id: &CredentialID) -> Option<Credential> {
        let cred = {
            if let Some(credential) = self.credentials.remove(id) {
                if let Some(by_element_entry) =
                    self.credential_ids_by_element.get_mut(&credential.element)
                {
                    by_element_entry.retain(|cid| cid != id);
                } else {
                    tracing::error!(
                        "missing {:?} in credential_ids_by_element",
                        &credential.element
                    );
                };
                Some(credential)
            } else {
                None
            }
        };
        self.tokens.remove(id);
        cred
    }

    fn for_element(&self, element: &ElementID) -> Vec<CredentialID> {
        let Some(credential_ids) = self.credential_ids_by_element.get(element) else {
            return Vec::new();
        };
        credential_ids.into_iter().cloned().collect()
    }

    pub fn unregister_all_for_element(&mut self, element: &ElementID) {
        let credential_ids = self.for_element(element);
        for id in credential_ids {
            self.unregister_id(&id);
        }
    }
}

#[derive(Debug)]
pub enum RegisterCredentialsError {
    Internal,
    NotAuthorized,
}

impl From<RegisterCredentialsError> for fpb::RegisterCredentialsError {
    fn from(e: RegisterCredentialsError) -> Self {
        match e {
            RegisterCredentialsError::Internal => fpb::RegisterCredentialsError::Internal,
            RegisterCredentialsError::NotAuthorized => fpb::RegisterCredentialsError::NotAuthorized,
        }
    }
}

#[derive(Debug)]
pub enum UnregisterCredentialsError {
    NotAuthorized,
}

impl From<UnregisterCredentialsError> for fpb::UnregisterCredentialsError {
    fn from(e: UnregisterCredentialsError) -> Self {
        match e {
            UnregisterCredentialsError::NotAuthorized => {
                fpb::UnregisterCredentialsError::NotAuthorized
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn test_no_permissions() {
        let none = Permissions::empty();
        assert_eq!(none.contains(Permissions::READ_POWER_LEVEL), false);
        assert_eq!(none.contains(Permissions::MODIFY_POWER_LEVEL), false);
        assert_eq!(none.contains(Permissions::MODIFY_DEPENDENT), false);
        assert_eq!(none.contains(Permissions::MODIFY_DEPENDENCY), false);
        assert_eq!(none.contains(Permissions::MODIFY_CREDENTIAL), false);
        assert_eq!(none.contains(Permissions::REMOVE_ELEMENT), false);
    }

    #[fuchsia::test]
    fn test_all_permissions() {
        let all = Permissions::all();
        assert_eq!(all.contains(Permissions::READ_POWER_LEVEL), true);
        assert_eq!(all.contains(Permissions::MODIFY_POWER_LEVEL), true);
        assert_eq!(all.contains(Permissions::MODIFY_DEPENDENT), true);
        assert_eq!(all.contains(Permissions::MODIFY_DEPENDENCY), true);
        assert_eq!(all.contains(Permissions::MODIFY_CREDENTIAL), true);
        assert_eq!(all.contains(Permissions::REMOVE_ELEMENT), true);
    }

    #[fuchsia::test]
    fn test_some_permissions() {
        let some = Permissions::MODIFY_POWER_LEVEL
            | Permissions::MODIFY_DEPENDENT
            | Permissions::MODIFY_CREDENTIAL;
        assert_eq!(some.contains(Permissions::READ_POWER_LEVEL), false);
        assert_eq!(some.contains(Permissions::MODIFY_POWER_LEVEL), true);
        assert_eq!(some.contains(Permissions::MODIFY_DEPENDENT), true);
        assert_eq!(some.contains(Permissions::MODIFY_DEPENDENCY), false);
        assert_eq!(some.contains(Permissions::MODIFY_CREDENTIAL), true);
        assert_eq!(some.contains(Permissions::REMOVE_ELEMENT), false);
    }

    #[fuchsia::test]
    fn test_credential_authorizes() {
        let gold_credential = Credential {
            element: "Gold".into(),
            id: zx::Event::create().get_koid().expect("get_koid failed"),
            permissions: Permissions::READ_POWER_LEVEL
                | Permissions::MODIFY_DEPENDENT
                | Permissions::REMOVE_ELEMENT,
        };
        assert_eq!(
            gold_credential.authorizes(&"Gold".into(), &Permissions::READ_POWER_LEVEL),
            true
        );
        assert_eq!(
            gold_credential.authorizes(&"Gold".into(), &Permissions::MODIFY_POWER_LEVEL),
            false
        );
        assert_eq!(
            gold_credential.authorizes(&"Gold".into(), &Permissions::MODIFY_DEPENDENT),
            true
        );
        assert_eq!(
            gold_credential.authorizes(&"Gold".into(), &Permissions::MODIFY_DEPENDENCY),
            false
        );
        assert_eq!(
            gold_credential.authorizes(&"Gold".into(), &Permissions::MODIFY_CREDENTIAL),
            false
        );
        assert_eq!(gold_credential.authorizes(&"Gold".into(), &Permissions::REMOVE_ELEMENT), true);
        assert_eq!(
            gold_credential.authorizes(&"Platinum".into(), &Permissions::READ_POWER_LEVEL),
            false
        );
    }

    #[fuchsia::test]
    fn test_convert_fidl_credential_to_register() {
        let (broker_token, _) = zx::EventPair::create();
        let want_koid = broker_token.basic_info().expect("basic_info failed").koid;
        let fidl_credential = fpb::Credential {
            broker_token,
            permissions: Permissions::READ_POWER_LEVEL
                | Permissions::MODIFY_DEPENDENT
                | Permissions::REMOVE_ELEMENT,
        };
        let ctr: CredentialToRegister = fidl_credential.into();
        assert_eq!(
            ctr.broker_token.event_pair.basic_info().expect("basic_info failed").koid,
            want_koid
        );
        assert_eq!(ctr.permissions.contains(Permissions::READ_POWER_LEVEL), true);
        assert_eq!(ctr.permissions.contains(Permissions::MODIFY_POWER_LEVEL), false);
        assert_eq!(ctr.permissions.contains(Permissions::MODIFY_DEPENDENT), true);
        assert_eq!(ctr.permissions.contains(Permissions::MODIFY_DEPENDENCY), false);
        assert_eq!(ctr.permissions.contains(Permissions::MODIFY_CREDENTIAL), false);
        assert_eq!(ctr.permissions.contains(Permissions::REMOVE_ELEMENT), true);
    }

    #[fuchsia::test]
    fn test_register_unregister() {
        let mut registry = Registry::new();
        let (token_red_kryptonite, token_green_kryptonite) = zx::EventPair::create();
        let element_kryptonite: ElementID = "Kryptonite".into();
        let credential_to_register = CredentialToRegister {
            broker_token: token_red_kryptonite.into(),
            permissions: Permissions::READ_POWER_LEVEL
                | Permissions::MODIFY_POWER_LEVEL
                | Permissions::MODIFY_CREDENTIAL,
        };
        registry.register(&element_kryptonite, credential_to_register).expect("register failed");
        use fuchsia_zircon::HandleBased;
        let token_green_kryptonite_dup = token_green_kryptonite
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .expect("duplicate_handle failed");
        let credential = registry.lookup(token_green_kryptonite_dup.into()).unwrap();
        assert_ne!(
            credential.id,
            token_green_kryptonite.basic_info().expect("basic_info failed").koid
        );
        assert_eq!(credential.element, element_kryptonite);
        assert_eq!(credential.permissions.contains(Permissions::READ_POWER_LEVEL), true);
        assert_eq!(credential.permissions.contains(Permissions::MODIFY_POWER_LEVEL), true);
        assert_eq!(credential.permissions.contains(Permissions::MODIFY_DEPENDENT), false);
        assert_eq!(credential.permissions.contains(Permissions::MODIFY_DEPENDENCY), false);
        assert_eq!(credential.permissions.contains(Permissions::MODIFY_CREDENTIAL), true);
        assert_eq!(credential.permissions.contains(Permissions::REMOVE_ELEMENT), false);

        let unregistered = registry.unregister(&credential);
        assert_eq!(unregistered, Some(credential.clone()));
        let lookup_not_found = registry.lookup(token_green_kryptonite.into());
        assert_eq!(lookup_not_found, None);

        let extra_unregister = registry.unregister(&credential);
        assert_eq!(extra_unregister, None);
    }
}
