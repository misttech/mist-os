// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::constants::PKG_PATH;
use crate::model::component::{ComponentInstance, Package, WeakComponentInstance};
use ::routing::component_instance::ComponentInstanceInterface;
use ::routing::mapper::NoopRouteMapper;
use ::routing::{route_to_storage_decl, verify_instance_in_component_id_index};
use cm_rust::ComponentDecl;
use errors::CreateNamespaceError;
use fidl::endpoints::ClientEnd;
use fidl::prelude::*;
use fidl_fuchsia_io as fio;
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::StreamExt;
use sandbox::{Capability, Dict};
use serve_processargs::NamespaceBuilder;
use std::sync::Arc;
use tracing::warn;
use vfs::execution_scope::ExecutionScope;

/// Creates a component's namespace.
///
/// TODO(b/298106231): eventually this should only build a delivery map as
/// the program dict will be fetched from the resolved component state.
pub async fn create_namespace(
    package: Option<&Package>,
    component: &Arc<ComponentInstance>,
    decl: &ComponentDecl,
    program_input_dict: &Dict,
    scope: ExecutionScope,
) -> Result<NamespaceBuilder, CreateNamespaceError> {
    let not_found_sender = not_found_logging(component);
    let mut namespace = NamespaceBuilder::new(scope.clone(), not_found_sender);
    if let Some(package) = package {
        let pkg_dir = fuchsia_fs::directory::clone_no_describe(&package.package_dir, None)
            .map_err(CreateNamespaceError::ClonePkgDirFailed)?;
        add_pkg_directory(&mut namespace, pkg_dir)?;
    }

    for use_ in &decl.uses {
        if let cm_rust::UseDecl::Storage(decl) = use_ {
            if let Ok(source) =
                route_to_storage_decl(decl.clone(), component, &mut NoopRouteMapper).await
            {
                verify_instance_in_component_id_index(&source, component)
                    .await
                    .map_err(CreateNamespaceError::InstanceNotInInstanceIdIndex)?;
            }
        }
    }

    program_input_dict_to_namespace("", &mut namespace, program_input_dict)?;
    Ok(namespace)
}

/// Adds the package directory to the namespace under the path "/pkg".
fn add_pkg_directory(
    namespace: &mut NamespaceBuilder,
    pkg_dir: fio::DirectoryProxy,
) -> Result<(), CreateNamespaceError> {
    // TODO(https://fxbug.dev/42060182): Use Proxy::into_client_end when available.
    let client_end = ClientEnd::new(pkg_dir.into_channel().unwrap().into_zx_channel());
    let directory: sandbox::Directory = client_end.into();
    let path = cm_types::NamespacePath::new(PKG_PATH.to_str().unwrap()).unwrap();
    namespace.add_entry(Capability::Directory(directory), &path)?;
    Ok(())
}

/// Adds namespace entries for a component's program input dictionary.
fn program_input_dict_to_namespace(
    prefix: &str,
    namespace: &mut NamespaceBuilder,
    program_input_dict: &Dict,
) -> Result<(), CreateNamespaceError> {
    // Convert (the transformed) program_input_dict to namespace.
    //
    // Flatten the namespace as much as possible.
    //
    // For example, a dictionary that contains a dictionary at "data" that contains one directory
    // at "foo" should add a directory to the namespace at "/data/foo", not a directory at "/data".
    for (key, value) in program_input_dict.enumerate() {
        match value {
            Ok(Capability::Dictionary(d)) => {
                program_input_dict_to_namespace(&format!("{prefix}/{key}"), namespace, &d)?;
            }
            Ok(cap @ Capability::Directory(_)) => {
                namespace.add_entry(cap, &format!("{prefix}/{key}").parse().unwrap())?;
            }
            Ok(cap @ Capability::DirConnector(_)) => {
                namespace.add_entry(cap, &format!("{prefix}/{key}").parse().unwrap())?;
            }
            Ok(cap) => {
                namespace.add_object(cap, &format!("{prefix}/{key}").parse().unwrap())?;
            }
            Err(_) => {}
        }
    }

    Ok(())
}

fn not_found_logging(component: &Arc<ComponentInstance>) -> UnboundedSender<String> {
    let (sender, mut receiver) = unbounded();
    let component_for_logger: WeakComponentInstance = component.as_weak();

    component.nonblocking_task_group().spawn(async move {
        while let Some(path) = receiver.next().await {
            match component_for_logger.upgrade() {
                Ok(target) => {
                    target
                        .with_logger_as_default(|| {
                            warn!(
                                "No capability available at path {} for component {}, \
                                verify the component has the proper `use` declaration.",
                                path, target.moniker
                            );
                        })
                        .await;
                }
                Err(_) => {}
            }
        }
    });

    sender
}
