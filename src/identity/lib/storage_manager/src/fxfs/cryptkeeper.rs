// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fxfs::log_and_map_err::LogThen,
    anyhow::{anyhow, Error},
    fidl::endpoints::{ClientEnd, Proxy},
    fidl_fuchsia_component::{self as fcomponent, RealmMarker},
    fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_fxfs::{CryptManagementMarker, CryptMarker, CryptProxy, KeyPurpose},
    fidl_fuchsia_identity_account as faccount, fidl_fuchsia_io as fio,
    fuchsia_component::client::{
        connect_to_protocol, connect_to_protocol_at_dir_root, open_childs_exposed_directory,
    },
    hkdf::Hkdf,
    sha2::Sha256,
    tracing::warn,
    typed_builder::TypedBuilder,
};

#[derive(TypedBuilder)]
pub struct Args {
    // The name of the component collection within which crypt components are
    // launched. Defaults to "crypt".
    #[builder(default = "crypt".to_string())]
    crypt_collection_name: String,

    // The name for the crypt component instance. Defaults to "crypt".
    #[builder(default = "crypt".to_string())]
    crypt_component_name: String,
}

const CRYPT_CM_URL: &str = "#meta/fxfs-crypt.cm";
const DATA_KEY_INDEX: u8 = 0;
const METADATA_KEY_INDEX: u8 = 1;

// Whether or not the Crypt component is currently running or being shut down.
#[derive(Debug, PartialEq)]
enum State {
    Running,
    ShuttingDown,
}

// Performs HKDF (HMAC key derivation) expansion from a single input |key|,
// which is expected to already be cryptographically strong.
//
//   make_hkdf(IKM, info) -> OKM
//      where
//      - IKM    input key material; |key| in this function
//      - info   additional context; different values of |info| will produce distinct keys.
//      - OKM    output key material; the return value of this function.
fn hkdf_expand(key: &[u8], info: &[u8]) -> Result<[u8; 32], Error> {
    let hk = Hkdf::<Sha256>::from_prk(key).map_err(|_| anyhow!("Could not make HKDF from key."))?;
    let mut output_key_material = [0u8; 32];
    hk.expand(info, &mut output_key_material).map_err(|_| anyhow!("Key was wrong length."))?;
    Ok(output_key_material)
}

/// A helper which provides access to the underlying Crypt service.
/// Responsible for destroying the Crypt directory when asked.
///
/// FYI: CryptKeeper expects to instantiate an FXFS Crypt component in the same
/// package as the calling component. For now, see:
/// - //src/identity/lib/storage_manager/meta/fxfs.shard.cml
///   - for the expected capabilities and collections this component needs, and
/// - //src/identity/tests/storage_manager_integration/tests/fxfs.rs
///   - for an example (test) callsite for FxfsStorageManager and CryptKeeper.
pub struct CryptKeeper {
    directory: fio::DirectoryProxy,
    // Whether the crypt component is running or shutting down.
    state: State,
    // The name of the component collection within which crypt components are launched.
    crypt_collection_name: String,
    // The name for the crypt component.
    crypt_component_name: String,
}

impl CryptKeeper {
    // Destroys the crypt component.
    pub async fn destroy(&mut self) -> Result<(), faccount::Error> {
        // Swap out self.state for State::ShuttingDown. If the component wasn't
        // already shutting down, kick off Realm.destroy_child().
        if std::mem::replace(&mut self.state, State::ShuttingDown) != State::ShuttingDown {
            let proxy = connect_to_protocol::<RealmMarker>()
                .log_error_then("Connect to Realm protocol failed", faccount::Error::Resource)?;

            let () = proxy
                .destroy_child(&fdecl::ChildRef {
                    name: self.crypt_component_name.to_string(),
                    collection: Some(self.crypt_collection_name.to_string()),
                })
                .await
                .log_warn_then("Could not send destroy child request", faccount::Error::Resource)?
                .log_warn_then("Could not destroy child", faccount::Error::Resource)?;
        }

        Ok(())
    }

    // Creates a new CryptKeeper instance (and a new underlying Crypt child
    // component to serve it) given some initializing key.
    pub async fn new_from_key(args: Args, key: &[u8]) -> Result<CryptKeeper, faccount::Error> {
        let realm_proxy = connect_to_protocol::<RealmMarker>()
            .log_error_then("Connect to Realm protocol failed", faccount::Error::Resource)?;

        realm_proxy
            .create_child(
                &fdecl::CollectionRef { name: args.crypt_collection_name.clone() },
                &fdecl::Child {
                    name: Some(args.crypt_component_name.clone()),
                    url: Some(CRYPT_CM_URL.to_string()),
                    startup: Some(fdecl::StartupMode::Lazy),
                    environment: None,
                    ..Default::default()
                },
                fcomponent::CreateChildArgs::default(),
            )
            .await
            .log_warn_then("create_child FIDL failed", faccount::Error::Resource)?
            .log_warn_then("create_child failed", faccount::Error::Resource)?;

        let exposed_dir = open_childs_exposed_directory(
            args.crypt_component_name.clone(),
            Some(args.crypt_collection_name.clone()),
        )
        .await
        .log_warn_then("open_childs_exposed_directory failed", faccount::Error::Resource)?;

        let crypt_management_service = connect_to_protocol_at_dir_root::<CryptManagementMarker>(
            &exposed_dir,
        )
        .log_error_then("Connect to CryptManagement protocol failed", faccount::Error::Resource)?;

        let data_wrapping_key = hkdf_expand(key, /*index=*/ &[DATA_KEY_INDEX])
            .log_warn_then("compute wrapping_key failed", faccount::Error::Resource)?;
        crypt_management_service
            .add_wrapping_key(DATA_KEY_INDEX.into(), &data_wrapping_key)
            .await
            .log_warn_then("add_wrapping_key FIDL failed", faccount::Error::Resource)?
            .log_warn_then("add_wrapping_key failed", faccount::Error::Resource)?;

        let metadata_wrapping_key = hkdf_expand(key, /*index=*/ &[METADATA_KEY_INDEX])
            .log_warn_then("compute wrapping_key failed", faccount::Error::Resource)?;
        crypt_management_service
            .add_wrapping_key(METADATA_KEY_INDEX.into(), &metadata_wrapping_key)
            .await
            .log_warn_then("add_wrapping_key FIDL failed", faccount::Error::Resource)?
            .log_warn_then("add_wrapping_key failed", faccount::Error::Resource)?;

        crypt_management_service
            .set_active_key(KeyPurpose::Data, DATA_KEY_INDEX.into())
            .await
            .log_warn_then("set_active_key FIDL failed", faccount::Error::Resource)?
            .log_warn_then("set_active_key failed", faccount::Error::Resource)?;

        crypt_management_service
            .set_active_key(KeyPurpose::Metadata, METADATA_KEY_INDEX.into())
            .await
            .log_warn_then("set_active_key FIDL failed", faccount::Error::Resource)?
            .log_warn_then("set_active_key failed", faccount::Error::Resource)?;

        Ok(CryptKeeper {
            directory: exposed_dir,
            state: State::Running,
            crypt_collection_name: args.crypt_collection_name,
            crypt_component_name: args.crypt_component_name,
        })
    }

    // Returns a crypt proxy.
    pub fn crypt_proxy(&self) -> Result<CryptProxy, faccount::Error> {
        match self.state {
            State::ShuttingDown => {
                warn!("Cannot retrieve the crypt proxy, the CryptKeeper is shutting down.");
                Err(faccount::Error::Internal)
            }
            State::Running => connect_to_protocol_at_dir_root::<CryptMarker>(&self.directory)
                .log_error_then(
                    "connect to CryptMarker protocol failed",
                    faccount::Error::Resource,
                ),
        }
    }

    // Returns a crypt proxy (like .crypt_proxy() above), but one which has been
    // preconverted into ClientEnd<CryptMarker>; callsites to Volumes.Create
    // etc. expect this form of the proxy channel.
    pub fn crypt_client_end(&self) -> Result<ClientEnd<CryptMarker>, faccount::Error> {
        Ok(self
            .crypt_proxy()?
            .into_channel()
            .log_warn_then("Failed to turn crypt proxy into channel", faccount::Error::Resource)?
            .into_zx_channel()
            .into())
    }
}

impl Drop for CryptKeeper {
    // When CryptKeeper is dropped, attempt to destroy it.
    fn drop(&mut self) {
        #[allow(unknown_lints)]
        #[allow(clippy::let_underscore_future)] // TODO(fxbug.dev/117901)
        let _ = self.destroy();
    }
}
