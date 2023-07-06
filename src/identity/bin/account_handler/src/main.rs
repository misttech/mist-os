// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! AccountHandler manages the state of a single system account and its personae on a Fuchsia
//! device, and provides access to authentication tokens for Service Provider accounts associated
//! with the account.

#![deny(missing_docs)]
#![allow(clippy::enum_variant_names)]
#![allow(clippy::let_unit_value)]
#![allow(clippy::await_holding_refcell_ref)]
#![allow(clippy::from_over_into)]

mod account;
mod account_handler;
mod common;
mod inspect;
mod interaction;
mod interaction_lock_state;
mod persona;
mod pre_auth;
mod storage_lock_request;
mod storage_lock_state;
mod stored_account;
mod wrapped_key;

#[cfg(test)]
mod test_util;

use {
    crate::{account_handler::AccountHandler, common::AccountLifetime},
    account_common::AccountId,
    account_handler_structured_config::Config,
    anyhow::Error,
    fidl::endpoints::RequestStream,
    fidl_fuchsia_identity_authentication::Mechanism,
    fidl_fuchsia_io as fio,
    fidl_fuchsia_process_lifecycle::LifecycleRequestStream,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_fs::directory::open_in_namespace,
    fuchsia_inspect::Inspector,
    fuchsia_runtime::{self as fruntime, HandleInfo, HandleType},
    futures::StreamExt,
    lazy_static::lazy_static,
    std::sync::Arc,
    storage_manager::{
        fxfs::{Args as FxfsStorageManagerArgs, Fxfs as FxfsStorageManager},
        minfs::{disk::DevDiskManager, StorageManager as MinfsStorageManager},
        StorageManager, StorageManagerEnum, StorageManagerKind,
    },
    tracing::{error, info, warn},
};

const DATA_DIR: &str = "/data";

lazy_static! {
    static ref CONFIG: Config = Config::take_from_startup_handle();

    // Statically initialized config validation for CONFIG. If
    // config.storage_manager isn't one of {FXFS, MINFS}, it will panic
    // immediately rather than when get_storage_manager() is called.
    static ref STORAGE_MANAGER_KIND: StorageManagerKind = {
        match CONFIG.storage_manager.as_str() {
            FXFS => StorageManagerKind::Fxfs,
            MINFS => StorageManagerKind::Minfs,
            other => {
                let error = format!(
                "AccountHandler was configured with invalid argument: storage_manager={other:?}. \
                See the CML declaration for valid values."
            );
                warn!("{}", error);
                panic!("{}", error);
            }
        }
    };
}

// These values should match the source of truth in account_handler.cml's
// config.storage_manager.
const FXFS: &str = "FXFS";
const MINFS: &str = "MINFS";

fn set_up_lifecycle_watcher<SM>(account_handler: Arc<AccountHandler<SM>>) -> fasync::Task<()>
where
    SM: StorageManager<Key = [u8; 32]> + Send + Sync + 'static,
{
    let handle_info = HandleInfo::new(HandleType::Lifecycle, 0);
    let handle = fruntime::take_startup_handle(handle_info)
        .expect("must have been provided a lifecycle channel in procargs");
    let async_chan =
        fasync::Channel::from_channel(handle.into()).expect("Async channel conversion failed.");
    let req_stream = LifecycleRequestStream::from_channel(async_chan);

    fasync::Task::spawn(
        async move { account_handler.handle_requests_for_lifecycle(req_stream).await },
    )
}

fn get_dev_directory() -> fio::DirectoryProxy {
    open_in_namespace("/dev", fio::OpenFlags::empty()).expect("open /dev root for disk manager")
}

fn get_svc_directory() -> fio::DirectoryProxy {
    open_in_namespace("/svc", fio::OpenFlags::empty()).expect("open /svc root for disk manager")
}

fn get_storage_manager(account_id: &AccountId) -> StorageManagerEnum {
    match &*STORAGE_MANAGER_KIND {
        StorageManagerKind::Fxfs => StorageManagerEnum::Fxfs(FxfsStorageManager::new(
            FxfsStorageManagerArgs::builder()
                .volume_label(account_id.to_canonical_string())
                .filesystem_dir(get_svc_directory())
                .build(),
        )),
        StorageManagerKind::Minfs => StorageManagerEnum::Minfs(MinfsStorageManager::new(
            DevDiskManager::new(get_dev_directory()),
        )),
    }
}

fn get_available_mechanisms() -> Vec<Mechanism> {
    CONFIG
        .available_mechanisms
        .iter()
        .map(|mechanism| match mechanism.as_str() {
            "TEST" => Mechanism::Test,
            "PASSWORD" => Mechanism::Password,
            other => {
                let error =
                    format!("AccountHandler was configured with invalid mechanism: {other:?}.");
                warn!("{}", error);
                panic!("{}", error);
            }
        })
        .collect()
}

fn main() -> Result<(), Error> {
    let lifetime = if CONFIG.is_ephemeral {
        AccountLifetime::Ephemeral
    } else {
        AccountLifetime::Persistent { account_dir: DATA_DIR.into() }
    };

    let mut executor = fasync::LocalExecutor::new();

    diagnostics_log::initialize(
        diagnostics_log::PublishOptions::default().tags(&["identity", "account_handler"]),
    )
    .unwrap();

    info!("Starting account handler");

    let inspector = Inspector::default();
    let mut fs = ServiceFs::new();
    inspect_runtime::serve(&inspector, &mut fs)?;

    let account_handler = Arc::new(AccountHandler::new(
        lifetime,
        &inspector,
        get_available_mechanisms(),
        Box::new(get_storage_manager),
    ));

    let _lifecycle_task: fuchsia_async::Task<()> =
        set_up_lifecycle_watcher(Arc::clone(&account_handler));

    fs.dir("svc").add_fidl_service(move |stream| {
        let account_handler_clone = Arc::clone(&account_handler);
        fasync::Task::spawn(async move {
            account_handler_clone
                .handle_requests_from_stream(stream)
                .await
                .unwrap_or_else(|err| error!(?err, "Error handling AccountHandlerControl channel"))
        })
        .detach();
    });
    fs.take_and_serve_directory_handle()?;

    executor.run_singlethreaded(fs.collect::<()>());

    info!("Stopping account handler");
    Ok(())
}
