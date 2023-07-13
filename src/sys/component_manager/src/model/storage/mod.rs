// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod admin_protocol;
use {
    crate::{
        capability::CapabilitySource,
        model::{
            component::{ComponentInstance, StartReason},
            error::ModelError,
            routing::{Route, RouteSource},
        },
    },
    ::routing::{
        capability_source::ComponentCapability, component_id_index::ComponentInstanceId,
        component_instance::ComponentInstanceInterface, error::RoutingError, RouteRequest,
    },
    anyhow::Error,
    clonable_error::ClonableError,
    cm_moniker::InstancedMoniker,
    derivative::Derivative,
    fidl::endpoints,
    fidl_fuchsia_io as fio,
    moniker::MonikerBase,
    std::{
        path::{Path, PathBuf},
        sync::Arc,
    },
    thiserror::Error,
};

// TODO: The `use` declaration for storage implicitly carries these rights. While this is
// correct, it would be more consistent to get the rights from `CapabilityState`.
const FLAGS: fio::OpenFlags = fio::OpenFlags::empty()
    .union(fio::OpenFlags::RIGHT_READABLE)
    .union(fio::OpenFlags::RIGHT_WRITABLE);

/// Information returned by the route_storage_capability function on the backing directory source
/// of a storage capability.
#[derive(Debug, Derivative)]
#[derivative(Clone(bound = ""))]
pub struct BackingDirectoryInfo {
    /// The component that's providing the backing directory capability for this storage
    /// capability. If None, then the backing directory comes from component_manager's namespace.
    pub storage_provider: Option<Arc<ComponentInstance>>,

    /// The path to the backing directory in the providing component's outgoing directory (or
    /// component_manager's namespace).
    pub backing_directory_path: cm_types::Path,

    /// The subdirectory inside of the backing directory capability to use, if any
    pub backing_directory_subdir: Option<PathBuf>,

    /// The subdirectory inside of the backing directory's sub-directory to use, if any. The
    /// difference between this and backing_directory_subdir is that backing_directory_subdir is
    /// appended to backing_directory_path first, and component_manager will create this subdir if
    /// it doesn't exist but won't create backing_directory_subdir.
    pub storage_subdir: Option<PathBuf>,

    /// The moniker of the component that defines the storage capability, with instance ids. This
    /// is used for generating moniker-based storage paths.
    pub storage_source_moniker: InstancedMoniker,
}

impl PartialEq for BackingDirectoryInfo {
    fn eq(&self, other: &Self) -> bool {
        let self_source_component = self.storage_provider.as_ref().map(|s| s.moniker());
        let other_source_component = other.storage_provider.as_ref().map(|s| s.moniker());
        self_source_component == other_source_component
            && self.backing_directory_path == other.backing_directory_path
            && self.backing_directory_subdir == other.backing_directory_subdir
            && self.storage_subdir == other.storage_subdir
    }
}

/// Errors related to isolated storage.
#[derive(Debug, Error, Clone)]
pub enum StorageError {
    #[error("failed to open {:?}'s directory {}: {} ", dir_source_moniker, dir_source_path, err)]
    OpenRoot {
        dir_source_moniker: Option<InstancedMoniker>,
        dir_source_path: cm_types::Path,
        #[source]
        err: ClonableError,
    },
    #[error(
        "failed to open isolated storage from {:?}'s directory {} for {} (instance_id={:?}): {} ",
        dir_source_moniker,
        dir_source_path,
        moniker,
        instance_id,
        err
    )]
    Open {
        dir_source_moniker: Option<InstancedMoniker>,
        dir_source_path: cm_types::Path,
        moniker: InstancedMoniker,
        instance_id: Option<ComponentInstanceId>,
        #[source]
        err: ClonableError,
    },
    #[error(
        "failed to open isolated storage from {:?}'s directory {} for {:?}: {} ",
        dir_source_moniker,
        dir_source_path,
        instance_id,
        err
    )]
    OpenById {
        dir_source_moniker: Option<InstancedMoniker>,
        dir_source_path: cm_types::Path,
        instance_id: ComponentInstanceId,
        #[source]
        err: ClonableError,
    },
    #[error(
        "failed to remove isolated storage from {:?}'s directory {} for {} (instance_id={:?}): {} ",
        dir_source_moniker,
        dir_source_path,
        moniker,
        instance_id,
        err
    )]
    Remove {
        dir_source_moniker: Option<InstancedMoniker>,
        dir_source_path: cm_types::Path,
        moniker: InstancedMoniker,
        instance_id: Option<ComponentInstanceId>,
        #[source]
        err: ClonableError,
    },
    #[error("storage path for moniker={}, instance_id={:?} is invalid", moniker, instance_id)]
    InvalidStoragePath { moniker: InstancedMoniker, instance_id: Option<ComponentInstanceId> },
}

impl StorageError {
    pub fn open_root(
        dir_source_moniker: Option<InstancedMoniker>,
        dir_source_path: cm_types::Path,
        err: impl Into<Error>,
    ) -> Self {
        Self::OpenRoot { dir_source_moniker, dir_source_path, err: err.into().into() }
    }

    pub fn open(
        dir_source_moniker: Option<InstancedMoniker>,
        dir_source_path: cm_types::Path,
        moniker: InstancedMoniker,
        instance_id: Option<ComponentInstanceId>,
        err: impl Into<Error>,
    ) -> Self {
        Self::Open {
            dir_source_moniker,
            dir_source_path,
            moniker,
            instance_id,
            err: err.into().into(),
        }
    }

    pub fn open_by_id(
        dir_source_moniker: Option<InstancedMoniker>,
        dir_source_path: cm_types::Path,
        instance_id: ComponentInstanceId,
        err: impl Into<Error>,
    ) -> Self {
        Self::OpenById { dir_source_moniker, dir_source_path, instance_id, err: err.into().into() }
    }

    pub fn remove(
        dir_source_moniker: Option<InstancedMoniker>,
        dir_source_path: cm_types::Path,
        moniker: InstancedMoniker,
        instance_id: Option<ComponentInstanceId>,
        err: impl Into<Error>,
    ) -> Self {
        Self::Remove {
            dir_source_moniker,
            dir_source_path,
            moniker,
            instance_id,
            err: err.into().into(),
        }
    }

    pub fn invalid_storage_path(
        moniker: InstancedMoniker,
        instance_id: Option<ComponentInstanceId>,
    ) -> Self {
        Self::InvalidStoragePath { moniker, instance_id }
    }
}

async fn open_storage_root(
    storage_source_info: &BackingDirectoryInfo,
) -> Result<fio::DirectoryProxy, ModelError> {
    let (mut dir_proxy, local_server_end) =
        endpoints::create_proxy::<fio::DirectoryMarker>().expect("failed to create proxy");
    let full_backing_directory_path = match storage_source_info.backing_directory_subdir.as_ref() {
        Some(subdir) => storage_source_info.backing_directory_path.to_path_buf().join(subdir),
        None => storage_source_info.backing_directory_path.to_path_buf(),
    };
    if let Some(dir_source_component) = storage_source_info.storage_provider.as_ref() {
        // TODO(fxbug.dev/50716): This should be StartReason::AccessCapability, but we haven't
        // plumbed in all the details needed to use it.
        dir_source_component.start(&StartReason::StorageAdmin, None, vec![], vec![]).await?;
        let path = full_backing_directory_path
            .to_str()
            .ok_or_else(|| ModelError::path_is_not_utf8(full_backing_directory_path.clone()))?;
        dir_source_component
            .open_outgoing(
                FLAGS | fio::OpenFlags::DIRECTORY,
                path,
                &mut local_server_end.into_channel(),
            )
            .await?;
    } else {
        // If storage_source_info.storage_provider is None, the directory comes from component_manager's namespace
        let path = full_backing_directory_path
            .to_str()
            .ok_or_else(|| ModelError::path_is_not_utf8(full_backing_directory_path.clone()))?;
        fuchsia_fs::directory::open_channel_in_namespace(path, FLAGS, local_server_end).map_err(
            |e| {
                ModelError::from(StorageError::open_root(
                    None,
                    storage_source_info.backing_directory_path.clone(),
                    e,
                ))
            },
        )?;
    }
    if let Some(subdir) = storage_source_info.storage_subdir.as_ref() {
        dir_proxy = fuchsia_fs::directory::create_directory_recursive(
            &dir_proxy,
            subdir.to_str().ok_or(ModelError::path_is_not_utf8(subdir.clone()))?,
            FLAGS,
        )
        .await
        .map_err(|e| {
            ModelError::from(StorageError::open_root(
                storage_source_info
                    .storage_provider
                    .as_ref()
                    .map(|r| r.instanced_moniker().clone()),
                storage_source_info.backing_directory_path.clone(),
                e,
            ))
        })?;
    }
    Ok(dir_proxy)
}

/// Routes a backing directory for a storage capability to its source, returning the data needed to
/// open the storage capability.
///
/// If the capability is not allowed to be routed to the `target`, per the
/// [`crate::model::policy::GlobalPolicyChecker`], then an error is returned.
///
/// REQUIRES: `storage_source` is `ComponentCapability::Storage`.
pub async fn route_backing_directory(
    storage_source: CapabilitySource,
) -> Result<BackingDirectoryInfo, RoutingError> {
    let (storage_decl, storage_component) = match storage_source {
        CapabilitySource::Component {
            capability: ComponentCapability::Storage(storage_decl),
            component,
        } => (storage_decl, component.upgrade()?),
        r => unreachable!("unexpected storage source: {:?}", r),
    };

    let source = RouteRequest::StorageBackingDirectory(storage_decl.clone())
        .route(&storage_component)
        .await?;

    let (dir_source_path, dir_source_instance, relative_path) = match source {
        RouteSource {
            source: CapabilitySource::Component { capability, component },
            relative_path,
        } => (
            capability.source_path().expect("directory has no source path?").clone(),
            Some(component.upgrade()?),
            relative_path,
        ),
        RouteSource { source: CapabilitySource::Namespace { capability, .. }, relative_path } => (
            capability.source_path().expect("directory has no source path?").clone(),
            None,
            relative_path,
        ),
        _ => unreachable!("not valid sources"),
    };
    let dir_subdir =
        if relative_path == Path::new("") { None } else { Some(relative_path.clone()) };

    Ok(BackingDirectoryInfo {
        storage_provider: dir_source_instance,
        backing_directory_path: dir_source_path,
        backing_directory_subdir: dir_subdir,
        storage_subdir: storage_decl.subdir.clone(),
        storage_source_moniker: storage_component.instanced_moniker().clone(),
    })
}

/// Open the isolated storage sub-directory from the given storage capability source, creating it
/// if necessary. The storage sub-directory is based on provided instance ID if present, otherwise
/// it is based on the provided relative moniker.
pub async fn open_isolated_storage(
    storage_source_info: &BackingDirectoryInfo,
    persistent_storage: bool,
    moniker: InstancedMoniker,
    instance_id: Option<&ComponentInstanceId>,
) -> Result<fio::DirectoryProxy, ModelError> {
    let root_dir = open_storage_root(storage_source_info).await?;
    let storage_path = match instance_id {
        Some(id) => generate_instance_id_based_storage_path(id),
        // if persistent_storage is `true`, generate a moniker-based storage path that ignores
        // instance ids.
        None => {
            if persistent_storage {
                generate_moniker_based_storage_path(&moniker.with_zero_value_instance_ids())
            } else {
                generate_moniker_based_storage_path(&moniker)
            }
        }
    };

    fuchsia_fs::directory::create_directory_recursive(
        &root_dir,
        storage_path.to_str().ok_or(ModelError::path_is_not_utf8(storage_path.clone()))?,
        FLAGS,
    )
    .await
    .map_err(|e| {
        ModelError::from(StorageError::open(
            storage_source_info.storage_provider.as_ref().map(|r| r.instanced_moniker().clone()),
            storage_source_info.backing_directory_path.clone(),
            moniker.clone(),
            instance_id.cloned(),
            e,
        ))
    })
}

/// Open the isolated storage sub-directory from the given storage capability source, creating it
/// if necessary. The storage sub-directory is based on provided instance ID.
pub async fn open_isolated_storage_by_id(
    storage_source_info: &BackingDirectoryInfo,
    instance_id: ComponentInstanceId,
) -> Result<fio::DirectoryProxy, ModelError> {
    let root_dir = open_storage_root(storage_source_info).await?;
    let storage_path = generate_instance_id_based_storage_path(&instance_id);

    fuchsia_fs::directory::create_directory_recursive(
        &root_dir,
        storage_path.to_str().ok_or(ModelError::path_is_not_utf8(storage_path.clone()))?,
        FLAGS,
    )
    .await
    .map_err(|e| {
        ModelError::from(StorageError::open_by_id(
            storage_source_info.storage_provider.as_ref().map(|r| r.instanced_moniker().clone()),
            storage_source_info.backing_directory_path.clone(),
            instance_id,
            e,
        ))
    })
}

/// Delete the isolated storage sub-directory for the given component.  `dir_source_component` and
/// `dir_source_path` are the component hosting the directory and its capability path. Note that
/// this removes the backing storage directory, meaning if this is called while the using
/// component is still alive, that component's storage handle will start returning errors.
pub async fn delete_isolated_storage(
    storage_source_info: BackingDirectoryInfo,
    persistent_storage: bool,
    moniker: InstancedMoniker,
    instance_id: Option<&ComponentInstanceId>,
) -> Result<(), ModelError> {
    let root_dir = open_storage_root(&storage_source_info).await?;

    let (dir, name) = if let Some(instance_id) = instance_id {
        let storage_path = generate_instance_id_based_storage_path(instance_id);
        let file_name = storage_path
            .file_name()
            .ok_or_else(|| {
                StorageError::invalid_storage_path(moniker.clone(), Some(instance_id.clone()))
            })?
            .to_str()
            .ok_or_else(|| ModelError::path_is_not_utf8(storage_path.clone()))?
            .to_string();

        let parent_path = storage_path.parent().ok_or_else(|| {
            StorageError::invalid_storage_path(moniker.clone(), Some(instance_id.clone()))
        })?;
        let parent_path_str = parent_path
            .to_str()
            .ok_or_else(|| ModelError::path_is_not_utf8(storage_path.clone()))?;
        let dir = if parent_path_str.is_empty() {
            root_dir
        } else {
            fuchsia_fs::directory::open_directory_no_describe(&root_dir, parent_path_str, FLAGS)
                .map_err(|e| {
                    StorageError::open(
                        storage_source_info
                            .storage_provider
                            .as_ref()
                            .map(|r| r.instanced_moniker().clone()),
                        storage_source_info.backing_directory_path.clone(),
                        moniker.clone(),
                        None,
                        e,
                    )
                })?
        };
        (dir, file_name)
    } else {
        let storage_path = if persistent_storage {
            generate_moniker_based_storage_path(&moniker.with_zero_value_instance_ids())
        } else {
            generate_moniker_based_storage_path(&moniker)
        };
        // We want to strip off the "data" portion of the path, and then one more level to get to the
        // directory holding the target component's storage.
        let storage_path_parent = storage_path
            .parent()
            .ok_or_else(|| StorageError::invalid_storage_path(moniker.clone(), None))?;
        let dir_path = storage_path_parent
            .parent()
            .ok_or_else(|| StorageError::invalid_storage_path(moniker.clone(), None))?;
        let name = storage_path_parent
            .file_name()
            .ok_or_else(|| StorageError::invalid_storage_path(moniker.clone(), None))?;
        let name =
            name.to_str().ok_or_else(|| ModelError::path_is_not_utf8(storage_path.clone()))?;
        let dir = if dir_path.parent().is_none() {
            root_dir
        } else {
            fuchsia_fs::directory::open_directory_no_describe(
                &root_dir,
                dir_path.to_str().unwrap(),
                FLAGS,
            )
            .map_err(|e| {
                StorageError::open(
                    storage_source_info
                        .storage_provider
                        .as_ref()
                        .map(|r| r.instanced_moniker().clone()),
                    storage_source_info.backing_directory_path.clone(),
                    moniker.clone(),
                    None,
                    e,
                )
            })?
        };
        (dir, name.to_string())
    };

    // TODO(fxbug.dev/36377): This function is subject to races. If another process has a handle to the
    // isolated storage directory, it can add files while this function is running. That could
    // cause it to spin or fail because a subdir was not empty after it removed all the contents.
    // It's also possible that the directory was already deleted by the backing component or a
    // prior run.
    fuchsia_fs::directory::remove_dir_recursive(&dir, &name).await.map_err(|e| {
        StorageError::remove(
            storage_source_info.storage_provider.as_ref().map(|r| r.instanced_moniker().clone()),
            storage_source_info.backing_directory_path.clone(),
            moniker.clone(),
            instance_id.cloned(),
            e,
        )
    })?;
    Ok(())
}

/// Generates the path into a directory the provided component will be afforded for storage
///
/// The path of the sub-directory for a component that uses a storage capability is based on each
/// component instance's child moniker as given in the `children` section of its parent's manifest,
/// for each component instance in the path from the `storage` declaration to the
/// `use` declaration.
///
/// These names are used as path elements, separated by elements of the name "children". The
/// string "data" is then appended to this path for compatibility reasons.
///
/// For example, if the following component instance tree exists, with `a` declaring storage
/// capabilities, and then storage being offered down the chain to `d`:
///
/// ```
///  a  <- declares storage "cache", offers "cache" to b
///  |
///  b  <- offers "cache" to c
///  |
///  c  <- offers "cache" to d
///  |
///  d  <- uses "cache" storage as `/my_cache`
/// ```
///
/// When `d` attempts to access `/my_cache` the framework creates the sub-directory
/// `b:0/children/c:0/children/d:0/data` in the directory used by `a` to declare storage
/// capabilities.  Then, the framework gives 'd' access to this new directory.
fn generate_moniker_based_storage_path(moniker: &InstancedMoniker) -> PathBuf {
    assert!(
        !moniker.path().is_empty(),
        "storage capability appears to have been exposed or used by its source"
    );

    let mut path = moniker.path().iter();
    let mut dir_path = vec![path.next().unwrap().to_string()];
    while let Some(p) = path.next() {
        dir_path.push("children".to_string());
        dir_path.push(p.to_string());
    }

    // Storage capabilities used to have a hardcoded set of types, which would be appended
    // here. To maintain compatibility with the old paths (and thus not lose data when this was
    // migrated) we append "data" here. This works because this is the only type of storage
    // that was actually used in the wild.
    //
    // This is only temporary, until the storage instance id migration changes this layout.
    dir_path.push("data".to_string());
    dir_path.into_iter().collect()
}

/// Generates the component storage directory path for the provided component instance.
///
/// Components which do not have an instance ID use a generate moniker-based storage path instead.
fn generate_instance_id_based_storage_path(instance_id: &ComponentInstanceId) -> PathBuf {
    instance_id.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use {
        crate::model::testing::{
            routing_test_helpers::{RoutingTest, RoutingTestBuilder},
            test_helpers::{self, component_decl_with_test_runner},
        },
        assert_matches::assert_matches,
        cm_moniker::InstancedMoniker,
        cm_rust::*,
        cm_rust_testing::ComponentDeclBuilder,
        component_id_index, fidl_fuchsia_io as fio,
        moniker::{Moniker, MonikerBase},
        rand::{self, distributions::Alphanumeric, Rng},
        std::{
            convert::{TryFrom, TryInto},
            str::FromStr,
            sync::Arc,
        },
    };

    #[fuchsia::test]
    async fn open_isolated_storage_test() {
        let components = vec![
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").add_lazy_child("c").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDecl {
                        name: "data".parse().unwrap(),
                        source_path: Some("/data".parse().unwrap()),
                        rights: fio::RW_STAR_DIR,
                    })
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source_name: "data".parse().unwrap(),
                        source: ExposeSource::Self_,
                        target_name: "data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::Operations::CONNECT),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let test = RoutingTest::new("a", components).await;
        let b_component = test
            .model
            .look_up(&vec!["b"].try_into().unwrap())
            .await
            .expect("failed to find component for b:0");
        let dir_source_path: cm_types::Path = "/data".parse().unwrap();
        let moniker = InstancedMoniker::try_from(vec!["c:0", "coll:d:1"]).unwrap();

        // Open.
        let dir = open_isolated_storage(
            &BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path.clone(),
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: InstancedMoniker::root(),
            },
            false,
            moniker.clone(),
            None,
        )
        .await
        .expect("failed to open isolated storage");
        assert_eq!(test_helpers::list_directory(&dir).await, Vec::<String>::new());
        test_helpers::write_file(&dir, "file", "hippos").await;
        assert_eq!(test_helpers::list_directory(&dir).await, vec!["file".to_string()]);

        // Open again.
        let dir = open_isolated_storage(
            &BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path.clone(),
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: InstancedMoniker::root(),
            },
            false,
            moniker.clone(),
            None,
        )
        .await
        .expect("failed to open isolated storage");
        assert_eq!(test_helpers::list_directory(&dir).await, vec!["file".to_string()]);

        // Open another component's storage.
        let moniker = InstancedMoniker::try_from(vec!["c:0", "coll:d:1", "e:0"]).unwrap();
        let dir = open_isolated_storage(
            &BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path.clone(),
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: InstancedMoniker::root(),
            },
            false,
            moniker.clone(),
            None,
        )
        .await
        .expect("failed to open isolated storage");
        assert_eq!(test_helpers::list_directory(&dir).await, Vec::<String>::new());
        test_helpers::write_file(&dir, "file", "hippos").await;
        assert_eq!(test_helpers::list_directory(&dir).await, vec!["file".to_string()]);
    }

    #[fuchsia::test]
    async fn open_isolated_storage_instance_id() {
        let components = vec![
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").add_lazy_child("c").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDecl {
                        name: "data".parse().unwrap(),
                        source_path: Some("/data".parse().unwrap()),
                        rights: fio::RW_STAR_DIR,
                    })
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source_name: "data".parse().unwrap(),
                        source: ExposeSource::Self_,
                        target_name: "data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::Operations::CONNECT),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let test = RoutingTest::new("a", components).await;
        let b_component = test
            .model
            .look_up(&vec!["b"].try_into().unwrap())
            .await
            .expect("failed to find component for b:0");
        let dir_source_path: cm_types::Path = "/data".parse().unwrap();
        let moniker = InstancedMoniker::try_from(vec!["c:0", "coll:d:1"]).unwrap();

        // open the storage directory using instance ID.
        let instance_id: ComponentInstanceId = ComponentInstanceId::from_str(
            &component_id_index::gen_instance_id(&mut rand::thread_rng()),
        )
        .expect("generated instance ID could not be parsed into ComponentInstanceId");
        let mut dir = open_isolated_storage(
            &BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path.clone(),
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: InstancedMoniker::root(),
            },
            false,
            moniker.clone(),
            Some(&instance_id),
        )
        .await
        .expect("failed to open isolated storage");

        // ensure the directory is actually open before querying its parent about it.
        let _: Vec<_> = dir.query().await.expect("failed to open directory");

        // check that an instance-ID based directory was created:
        assert!(test_helpers::list_directory(&test.test_dir_proxy)
            .await
            .contains(&instance_id.to_string()));

        // check that a moniker-based directory was NOT created:
        assert!(!test_helpers::list_directory_recursive(&test.test_dir_proxy).await.contains(
            &generate_moniker_based_storage_path(&moniker).to_str().unwrap().to_string()
        ));

        // check that the directory is writable by writing a marker file in it.
        let marker_file_name: String =
            rand::thread_rng().sample_iter(&Alphanumeric).take(7).map(char::from).collect();
        assert_eq!(test_helpers::list_directory(&dir).await, Vec::<String>::new());
        test_helpers::write_file(&dir, &marker_file_name, "contents").await;
        assert_eq!(test_helpers::list_directory(&dir).await, vec![marker_file_name.clone()]);

        // check that re-opening the directory gives us the same marker file.
        dir = open_isolated_storage(
            &BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path.clone(),
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: InstancedMoniker::root(),
            },
            false,
            moniker.clone(),
            Some(&instance_id),
        )
        .await
        .expect("failed to open isolated storage");

        assert_eq!(test_helpers::list_directory(&dir).await, vec![marker_file_name.clone()]);
    }

    // TODO: test with different subdirs

    #[fuchsia::test]
    async fn open_isolated_storage_failure_test() {
        let components = vec![("a", component_decl_with_test_runner())];

        // Create a universe with a single component, whose outgoing directory service
        // simply closes the channel of incoming requests.
        let test = RoutingTestBuilder::new("a", components)
            .set_component_outgoing_host_fn("a", Box::new(|_| {}))
            .build()
            .await;
        test.start_instance_and_wait_start(&Moniker::root()).await.unwrap();

        // Try to open the storage. We expect an error.
        let moniker = InstancedMoniker::try_from(vec!["c:0", "coll:d:1"]).unwrap();
        let res = open_isolated_storage(
            &BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&test.model.root())),
                backing_directory_path: "/data".parse().unwrap(),
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: InstancedMoniker::root(),
            },
            false,
            moniker.clone(),
            None,
        )
        .await;
        assert_matches!(res, Err(ModelError::StorageError { err: StorageError::Open { .. } }));
    }

    #[fuchsia::test]
    async fn delete_isolated_storage_test() {
        let components = vec![
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").add_lazy_child("c").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDecl {
                        name: "data".parse().unwrap(),
                        source_path: Some("/data".parse().unwrap()),
                        rights: fio::RW_STAR_DIR,
                    })
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source_name: "data".parse().unwrap(),
                        source: ExposeSource::Self_,
                        target_name: "data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::Operations::CONNECT),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let test = RoutingTest::new("a", components).await;
        let b_component = test
            .model
            .look_up(&vec!["b"].try_into().unwrap())
            .await
            .expect("failed to find component for b:0");
        let dir_source_path: cm_types::Path = "/data".parse().unwrap();
        let storage_moniker = InstancedMoniker::try_from(vec!["c:0"]).unwrap();
        let parent_moniker = InstancedMoniker::try_from(vec!["c:0"]).unwrap();
        let child_moniker = InstancedMoniker::try_from(vec!["c:0", "coll:d:1"]).unwrap();

        // Open and write to the storage for child.
        let dir = open_isolated_storage(
            &BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path.clone(),
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: storage_moniker.clone(),
            },
            false,
            child_moniker.clone(),
            None,
        )
        .await
        .expect("failed to open isolated storage");
        assert_eq!(test_helpers::list_directory(&dir).await, Vec::<String>::new());
        test_helpers::write_file(&dir, "file", "hippos").await;
        assert_eq!(test_helpers::list_directory(&dir).await, vec!["file".to_string()]);

        // Open parent's storage.
        let dir = open_isolated_storage(
            &BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path.clone(),
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: storage_moniker.clone(),
            },
            false,
            parent_moniker.clone(),
            None,
        )
        .await
        .expect("failed to open isolated storage");
        assert_eq!(test_helpers::list_directory(&dir).await, Vec::<String>::new());
        test_helpers::write_file(&dir, "file", "hippos").await;
        assert_eq!(test_helpers::list_directory(&dir).await, vec!["file".to_string()]);

        // Delete the child's storage.
        delete_isolated_storage(
            BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path.clone(),
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: storage_moniker.clone(),
            },
            false,
            child_moniker.clone(),
            None,
        )
        .await
        .expect("failed to delete child's isolated storage");

        // Open parent's storage again. Should work.
        let dir = open_isolated_storage(
            &BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path.clone(),
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: storage_moniker.clone(),
            },
            false,
            parent_moniker.clone(),
            None,
        )
        .await
        .expect("failed to open isolated storage");
        assert_eq!(test_helpers::list_directory(&dir).await, vec!["file".to_string()]);

        // Open list of children from parent. Should not contain child directory.
        assert_eq!(
            test.list_directory_in_storage(None, parent_moniker.clone(), None, "children").await,
            Vec::<String>::new(),
        );

        // Error -- tried to delete nonexistent storage.
        let err = delete_isolated_storage(
            BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path,
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: storage_moniker.clone(),
            },
            false,
            child_moniker.clone(),
            None,
        )
        .await
        .expect_err("delete isolated storage not meant to succeed");
        match err {
            ModelError::StorageError { err: StorageError::Remove { .. } } => {}
            _ => {
                panic!("unexpected error: {:?}", err);
            }
        }
    }

    #[fuchsia::test]
    async fn delete_isolated_storage_instance_id_test() {
        let components = vec![
            ("a", ComponentDeclBuilder::new().add_lazy_child("b").add_lazy_child("c").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .directory(DirectoryDecl {
                        name: "data".parse().unwrap(),
                        source_path: Some("/data".parse().unwrap()),
                        rights: fio::RW_STAR_DIR,
                    })
                    .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                        source_name: "data".parse().unwrap(),
                        source: ExposeSource::Self_,
                        target_name: "data".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        rights: Some(fio::Operations::CONNECT),
                        subdir: None,
                        availability: cm_rust::Availability::Required,
                    }))
                    .build(),
            ),
        ];
        let test = RoutingTest::new("a", components).await;
        let b_component = test
            .model
            .look_up(&vec!["b"].try_into().unwrap())
            .await
            .expect("failed to find component for b:0");
        let dir_source_path: cm_types::Path = "/data".parse().unwrap();
        let parent_moniker = InstancedMoniker::try_from(vec!["c:0"]).unwrap();
        let child_moniker = InstancedMoniker::try_from(vec!["c:0", "coll:d:1"]).unwrap();
        let instance_id = ComponentInstanceId::from_str(&component_id_index::gen_instance_id(
            &mut rand::thread_rng(),
        ))
        .expect("generated ID could not be parsed into component instance ID");
        // Open and write to the storage for child.
        let dir = open_isolated_storage(
            &BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path.clone(),
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: parent_moniker.clone(),
            },
            false,
            child_moniker.clone(),
            Some(&instance_id),
        )
        .await
        .expect("failed to open isolated storage");

        // ensure the directory is actually open before querying its parent about it.
        let _: Vec<_> = dir.query().await.expect("failed to open directory");

        // check that an instance-ID based directory was created:
        assert!(test_helpers::list_directory(&test.test_dir_proxy)
            .await
            .contains(&instance_id.to_string()));

        assert_eq!(test_helpers::list_directory(&dir).await, Vec::<String>::new());
        test_helpers::write_file(&dir, "file", "hippos").await;
        assert_eq!(test_helpers::list_directory(&dir).await, vec!["file".to_string()]);

        // Delete the child's storage.
        delete_isolated_storage(
            BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path.clone(),
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: parent_moniker.clone(),
            },
            false,
            child_moniker.clone(),
            Some(&instance_id),
        )
        .await
        .expect("failed to delete child's isolated storage");

        // check that an instance-ID based directory was deleted:
        assert!(!test_helpers::list_directory(&test.test_dir_proxy)
            .await
            .contains(&instance_id.to_string()));

        // Error -- tried to delete nonexistent storage.
        let err = delete_isolated_storage(
            BackingDirectoryInfo {
                storage_provider: Some(Arc::clone(&b_component)),
                backing_directory_path: dir_source_path,
                backing_directory_subdir: None,
                storage_subdir: None,
                storage_source_moniker: parent_moniker.clone(),
            },
            false,
            child_moniker,
            Some(&instance_id),
        )
        .await
        .expect_err("delete isolated storage not meant to succeed");
        match err {
            ModelError::StorageError { err: StorageError::Remove { .. } } => {}
            _ => {
                panic!("unexpected error: {:?}", err);
            }
        }
    }

    #[fuchsia::test]
    fn generate_moniker_based_storage_path_test() {
        for (moniker, expected_output) in vec![
            (vec!["a:1"].try_into().unwrap(), "a:1/data"),
            (vec!["a:1", "b:2"].try_into().unwrap(), "a:1/children/b:2/data"),
            (vec!["a:1", "b:2", "c:3"].try_into().unwrap(), "a:1/children/b:2/children/c:3/data"),
        ] {
            assert_eq!(
                generate_moniker_based_storage_path(&moniker),
                PathBuf::from(expected_output)
            )
        }
    }
}
