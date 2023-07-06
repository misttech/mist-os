// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(clippy::enum_variant_names)]
#![allow(clippy::let_unit_value)]
#![allow(clippy::expect_fun_call)]
#![allow(clippy::await_holding_refcell_ref)]

mod account;
mod account_manager;
mod account_metadata;
mod keys;
mod password_interaction;
mod pinweaver;
mod policy;
mod scrypt;
mod state;
mod storage_unlock_mechanism;

use {
    crate::{
        account_manager::AccountManager, account_metadata::DataDirAccountMetadataStore,
        pinweaver::EnvCredManagerProvider, storage_unlock_mechanism::StorageUnlockMechanism,
    },
    anyhow::{anyhow, Context, Error},
    fidl::endpoints::RequestStream,
    fidl_fuchsia_identity_account::AccountManagerRequestStream,
    fidl_fuchsia_identity_authentication::StorageUnlockMechanismRequestStream,
    fidl_fuchsia_io as fio,
    fidl_fuchsia_process_lifecycle::LifecycleRequestStream,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_fs::directory::open_in_namespace,
    fuchsia_runtime::{self as fruntime, duplicate_utc_clock_handle, HandleInfo, HandleType},
    fuchsia_zircon as zx,
    futures::StreamExt,
    std::sync::Arc,
    storage_manager::minfs::{disk::DevDiskManager, StorageManager as MinfsStorageManager},
    tracing::{error, info},
};

/// PasswordAuthenticator config, populated from a build-time generated
/// config.
#[derive(Clone)]
pub struct Config {
    pub allow_scrypt: bool,
    pub allow_pinweaver: bool,
}

impl From<password_authenticator_config::Config> for Config {
    fn from(source: password_authenticator_config::Config) -> Self {
        Config { allow_scrypt: source.allow_scrypt, allow_pinweaver: source.allow_pinweaver }
    }
}

enum Services {
    AccountManager(AccountManagerRequestStream),
    StorageUnlockMechanism(StorageUnlockMechanismRequestStream),
}

#[fuchsia::main(logging_tags = ["identity", "password_authenticator"])]
async fn main() -> Result<(), Error> {
    info!("Starting password authenticator");

    let config: Config = password_authenticator_config::Config::take_from_startup_handle().into();
    // validate that at least one account metadata type is enabled
    if !config.allow_scrypt && !config.allow_pinweaver {
        let err = anyhow!("No account types allowed by config, exiting");
        error!(%err);
        return Err(err);
    }

    let metadata_root = open_in_namespace(
        "/data/accounts",
        fio::OpenFlags::RIGHT_READABLE
            | fio::OpenFlags::RIGHT_WRITABLE
            | fio::OpenFlags::DIRECTORY
            | fio::OpenFlags::CREATE,
    )?;
    let mut account_metadata_store = DataDirAccountMetadataStore::new(metadata_root);
    // Clean up any not-committed files laying around in the account metadata directory.
    let cleanup_res = account_metadata_store.cleanup_stale_files().await;
    // If any cleanup fails, ignore it -- we can still perform our primary function with
    // stale files laying around.
    // TODO(zarvox): someday, make an inspect entry for this failure mode
    drop(cleanup_res);

    let cred_manager_provider = EnvCredManagerProvider {};

    let account_manager = Arc::new(AccountManager::new(
        config.clone(),
        account_metadata_store,
        cred_manager_provider,
        || {
            MinfsStorageManager::new(DevDiskManager::new(
                open_in_namespace("/dev", fio::OpenFlags::RIGHT_READABLE)
                    .expect("open /dev root for disk manager"),
            ))
        },
    ));

    // EnvCredManagerProvider is stateless and not shared.
    let cred_manager_provider = EnvCredManagerProvider {};

    let clock = duplicate_utc_clock_handle(zx::Rights::SAME_RIGHTS)
        .expect("Failed to duplicate UTC clock handle.");

    let storage_unlock_mechanism =
        Arc::new(StorageUnlockMechanism::new(config, cred_manager_provider, clock));

    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(Services::AccountManager);
    fs.dir("svc").add_fidl_service(Services::StorageUnlockMechanism);
    fs.take_and_serve_directory_handle().context("serving directory handle")?;

    let lifecycle_handle_info = HandleInfo::new(HandleType::Lifecycle, 0);
    let lifecycle_handle = fruntime::take_startup_handle(lifecycle_handle_info)
        .expect("must have been provided a lifecycle channel in procargs");
    let async_chan = fasync::Channel::from_channel(lifecycle_handle.into())
        .expect("Async channel conversion failed.");
    let lifecycle_req_stream = LifecycleRequestStream::from_channel(async_chan);

    let account_manager_for_lifecycle = Arc::clone(&account_manager);
    let _lifecycle_task = fasync::Task::spawn(async move {
        account_manager_for_lifecycle.handle_requests_for_lifecycle(lifecycle_req_stream).await
    });

    fs.for_each_concurrent(None, |service| async {
        match service {
            Services::AccountManager(stream) => {
                account_manager.handle_requests_for_account_manager(stream).await
            }
            Services::StorageUnlockMechanism(stream) => {
                storage_unlock_mechanism.handle_requests_for_storage_unlock_mechanism(stream).await
            }
        }
    })
    .await;

    Ok(())
}
