// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::io::Directory,
    anyhow::Result,
    fidl::endpoints::{create_proxy_and_stream, create_request_stream, ClientEnd, ServerEnd},
    fidl_fuchsia_component_decl as fcdecl, fidl_fuchsia_io as fio,
    fidl_fuchsia_io::DirectoryProxy,
    fidl_fuchsia_sys2 as fsys,
    fuchsia_async::Task,
    futures::StreamExt,
    std::collections::HashMap,
    std::fs::{create_dir, create_dir_all, metadata, set_permissions, write},
    std::path::PathBuf,
    tempfile::{tempdir, TempDir},
};

#[derive(Clone)]
pub struct File {
    pub on_host: bool,
    pub name: &'static str,
    pub data: &'static str,
}

#[derive(Clone)]
pub enum SeedPath {
    File(File),
    Directory(String),
}

fn serve_instance_iterator(
    instances: Vec<fsys::Instance>,
) -> ClientEnd<fsys::InstanceIteratorMarker> {
    let (client, mut stream) = create_request_stream::<fsys::InstanceIteratorMarker>().unwrap();
    Task::spawn(async move {
        let fsys::InstanceIteratorRequest::Next { responder } =
            stream.next().await.unwrap().unwrap();
        responder.send(&mut instances.into_iter()).unwrap();
        let fsys::InstanceIteratorRequest::Next { responder } =
            stream.next().await.unwrap().unwrap();
        responder.send(&mut std::iter::empty()).unwrap();
    })
    .detach();
    client
}

pub fn serve_realm_query_instances(instances: Vec<fsys::Instance>) -> fsys::RealmQueryProxy {
    serve_realm_query(instances, HashMap::new(), HashMap::new())
}

pub fn serve_realm_query(
    instances: Vec<fsys::Instance>,
    manifests: HashMap<String, fcdecl::Component>,
    dirs: HashMap<(String, fsys::OpenDirType), TempDir>,
) -> fsys::RealmQueryProxy {
    let (client, mut stream) = create_proxy_and_stream::<fsys::RealmQueryMarker>().unwrap();

    let mut instance_map = HashMap::new();
    for instance in instances {
        let moniker = instance.moniker.as_ref().unwrap().clone();
        let previous = instance_map.insert(moniker, instance);
        assert!(previous.is_none());
    }

    Task::spawn(async move {
        loop {
            match stream.next().await.unwrap().unwrap() {
                fsys::RealmQueryRequest::GetManifest { moniker, responder } => {
                    eprintln!("GetManifest call for {}", moniker);
                    if let Some(manifest) = manifests.get(&moniker) {
                        responder.send(&mut Ok(manifest.clone())).unwrap();
                    } else {
                        responder.send(&mut Err(fsys::GetManifestError::InstanceNotFound)).unwrap();
                    }
                }
                fsys::RealmQueryRequest::GetAllInstances { responder } => {
                    eprintln!("GetAllInstances call");
                    let instances = instance_map.values().cloned().collect();
                    let iterator = serve_instance_iterator(instances);
                    responder.send(&mut Ok(iterator)).unwrap();
                }
                fsys::RealmQueryRequest::Open {
                    moniker,
                    dir_type,
                    flags,
                    mode: _,
                    path,
                    object,
                    responder,
                } => {
                    eprintln!(
                        "Open call for {} for {:?} at path '{}' with flags {:?}",
                        moniker, dir_type, path, flags
                    );
                    if let Some(dir) = dirs.get(&(moniker, dir_type)) {
                        let path = dir.path().join(path).display().to_string();
                        fuchsia_fs::node::open_channel_in_namespace(&path, flags, object).unwrap();
                        responder.send(&mut Ok(())).unwrap();
                    } else {
                        responder.send(&mut Err(fsys::OpenError::NoSuchDir)).unwrap();
                    }
                }
                _ => panic!("Unexpected RealmQuery request"),
            }
        }
    })
    .detach();
    client
}

// Setup |RealmQuery| server with the given component instances.
pub fn serve_realm_query_deprecated(
    mut instances: HashMap<String, (fsys::InstanceInfo, Option<Box<fsys::ResolvedState>>)>,
) -> fsys::RealmQueryProxy {
    let (client, mut stream) = create_proxy_and_stream::<fsys::RealmQueryMarker>().unwrap();
    Task::spawn(async move {
        loop {
            let (moniker, responder) = match stream.next().await.unwrap().unwrap() {
                fsys::RealmQueryRequest::GetInstanceInfo { moniker, responder } => {
                    (moniker, responder)
                }
                _ => panic!("Unexpected RealmQuery request"),
            };

            let response = instances.remove(&moniker);

            match response {
                Some(instance) => responder.send(&mut Ok(instance)).unwrap(),
                None => responder.send(&mut Err(fsys::RealmQueryError::InstanceNotFound)).unwrap(),
            };
        }
    })
    .detach();
    client
}

// Set the permissions of a path on host to read only.
pub fn set_path_to_read_only(path: PathBuf) -> Result<()> {
    match metadata(path.clone()) {
        Ok(metadata) => {
            let mut perm = metadata.permissions();
            perm.set_readonly(true);
            set_permissions(path, perm).unwrap();
            Ok(())
        }
        Err(e) => panic!("Could not set path to read only: {}", e),
    }
}

// Create an arbitrary path string with tmp as the root.
pub fn create_tmp_path() -> String {
    let tmp_dir = tempdir().unwrap();
    let tmp_path = String::from(tmp_dir.path().to_str().unwrap());
    return tmp_path;
}

// Converts a vector of Files to a vector of SeedPaths.
pub fn generate_file_paths(file_paths: Vec<File>) -> Vec<SeedPath> {
    file_paths.iter().map(|file| SeedPath::File(file.to_owned())).collect::<Vec<SeedPath>>()
}

// Converts a vector of directory strs to a vector of SeedPaths.
pub fn generate_directory_paths(directory_paths: Vec<&str>) -> Vec<SeedPath> {
    directory_paths
        .iter()
        .map(|dir| SeedPath::Directory(dir.to_string()))
        .collect::<Vec<SeedPath>>()
}

// Create a new temporary directory to serve as the mock namespace.
pub fn serve_realm_query_with_namespace(
    server: ServerEnd<fio::DirectoryMarker>,
    seed_files: Vec<SeedPath>,
    is_read_only: bool,
) -> Result<()> {
    let tmp_path = create_tmp_path();
    let () = create_dir(&tmp_path).unwrap();

    for seed_path in seed_files {
        match seed_path {
            SeedPath::File(file) => {
                if !file.on_host {
                    let directory =
                        PathBuf::from(&file.name).parent().unwrap().to_string_lossy().to_string();
                    create_dir_all(format!("{}/{}", &tmp_path, &directory)).unwrap();
                    let final_path = format!("{}/{}", &tmp_path, &file.name);
                    write(&final_path, &file.data).unwrap();
                }
            }
            SeedPath::Directory(directory) => {
                create_dir(format!("{}/{}", &tmp_path, &directory)).unwrap()
            }
        }
    }

    let flags = if is_read_only {
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY
    } else {
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::DIRECTORY
    };

    fuchsia_fs::directory::open_channel_in_namespace(
        &tmp_path,
        flags,
        ServerEnd::new(server.into_channel()),
    )
    .unwrap();
    Ok(())
}

// Creates files with specified contents within a host directory.
pub fn populate_host_with_file_contents(path: &str, seed_files: Vec<SeedPath>) -> Result<()> {
    for seed_path in seed_files {
        match seed_path {
            SeedPath::File(file) => {
                if file.on_host {
                    let new_file_path = format!("{}/{}", path, &file.name);
                    write(&new_file_path, file.data.as_bytes().to_vec()).unwrap();
                }
            }
            _ => (),
        };
    }
    Ok(())
}

// Returns the data within a path in a namespace.
pub async fn read_data_from_namespace(
    ns_proxy: &DirectoryProxy,
    data_path: &str,
) -> Result<Vec<u8>> {
    let ns_dir = Directory::from_proxy(ns_proxy.to_owned());
    let file_data = ns_dir.read_file_bytes(PathBuf::from(data_path)).await.unwrap();
    Ok(file_data)
}
