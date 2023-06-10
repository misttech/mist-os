// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    anyhow::Result,
    fidl::endpoints::{create_proxy_and_stream, create_request_stream, ClientEnd},
    fidl_fuchsia_component_decl as fcdecl, fidl_fuchsia_sys2 as fsys,
    fuchsia_async::Task,
    futures::StreamExt,
    std::collections::HashMap,
    std::fs::{create_dir_all, write},
    tempfile::TempDir,
};

#[derive(Clone)]
pub struct File {
    pub name: &'static str,
    pub data: &'static str,
}

#[derive(Clone)]
pub enum SeedPath {
    File(File),
    Directory(&'static str),
}

fn serve_instance_iterator(
    instances: Vec<fsys::Instance>,
) -> ClientEnd<fsys::InstanceIteratorMarker> {
    let (client, mut stream) = create_request_stream::<fsys::InstanceIteratorMarker>().unwrap();
    Task::spawn(async move {
        let fsys::InstanceIteratorRequest::Next { responder } =
            stream.next().await.unwrap().unwrap();
        responder.send(&instances).unwrap();
        let fsys::InstanceIteratorRequest::Next { responder } =
            stream.next().await.unwrap().unwrap();
        responder.send(&[]).unwrap();
    })
    .detach();
    client
}

fn serve_manifest_bytes_iterator(
    manifest: fcdecl::Component,
) -> ClientEnd<fsys::ManifestBytesIteratorMarker> {
    let bytes = fidl::persist(&manifest).unwrap();
    let (client, mut stream) =
        create_request_stream::<fsys::ManifestBytesIteratorMarker>().unwrap();
    Task::spawn(async move {
        let fsys::ManifestBytesIteratorRequest::Next { responder } =
            stream.next().await.unwrap().unwrap();
        responder.send(&bytes).unwrap();
        let fsys::ManifestBytesIteratorRequest::Next { responder } =
            stream.next().await.unwrap().unwrap();
        responder.send(&[]).unwrap();
    })
    .detach();
    client
}

pub fn serve_realm_query_instances(instances: Vec<fsys::Instance>) -> fsys::RealmQueryProxy {
    serve_realm_query(instances, HashMap::new(), HashMap::new(), HashMap::new())
}

pub fn serve_realm_query(
    instances: Vec<fsys::Instance>,
    manifests: HashMap<String, fcdecl::Component>,
    configs: HashMap<String, fcdecl::ResolvedConfig>,
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
                fsys::RealmQueryRequest::GetInstance { moniker, responder } => {
                    eprintln!("GetInstance call for {}", moniker);
                    if let Some(instance) = instance_map.get(&moniker) {
                        responder.send(Ok(instance)).unwrap();
                    } else {
                        responder.send(Err(fsys::GetInstanceError::InstanceNotFound)).unwrap();
                    }
                }
                fsys::RealmQueryRequest::GetResolvedDeclaration { moniker, responder } => {
                    eprintln!("GetResolvedDeclaration call for {}", moniker);
                    if let Some(manifest) = manifests.get(&moniker) {
                        let iterator = serve_manifest_bytes_iterator(manifest.clone());
                        responder.send(Ok(iterator)).unwrap();
                    } else {
                        responder.send(Err(fsys::GetManifestError::InstanceNotFound)).unwrap();
                    }
                }
                fsys::RealmQueryRequest::GetManifest { moniker, responder } => {
                    eprintln!("GetManifest call for {}", moniker);
                    if let Some(manifest) = manifests.get(&moniker) {
                        let iterator = serve_manifest_bytes_iterator(manifest.clone());
                        responder.send(Ok(iterator)).unwrap();
                    } else {
                        responder.send(Err(fsys::GetManifestError::InstanceNotFound)).unwrap();
                    }
                }
                fsys::RealmQueryRequest::GetStructuredConfig { moniker, responder } => {
                    eprintln!("GetStructuredConfig call for {}", moniker);
                    if let Some(config) = configs.get(&moniker) {
                        responder.send(Ok(config)).unwrap();
                    } else {
                        responder
                            .send(Err(fsys::GetStructuredConfigError::InstanceNotFound))
                            .unwrap();
                    }
                }
                fsys::RealmQueryRequest::GetAllInstances { responder } => {
                    eprintln!("GetAllInstances call");
                    let instances = instance_map.values().cloned().collect();
                    let iterator = serve_instance_iterator(instances);
                    responder.send(Ok(iterator)).unwrap();
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
                        responder.send(Ok(())).unwrap();
                    } else {
                        responder.send(Err(fsys::OpenError::NoSuchDir)).unwrap();
                    }
                }
                _ => panic!("Unexpected RealmQuery request"),
            }
        }
    })
    .detach();
    client
}

// Converts a vector of Files to a vector of SeedPaths.
pub fn generate_file_paths(file_paths: Vec<File>) -> Vec<SeedPath> {
    file_paths.iter().map(|file| SeedPath::File(file.to_owned())).collect::<Vec<SeedPath>>()
}

// Converts a vector of directory strs to a vector of SeedPaths.
pub fn generate_directory_paths(directory_paths: Vec<&'static str>) -> Vec<SeedPath> {
    directory_paths.iter().map(|dir| SeedPath::Directory(dir)).collect::<Vec<SeedPath>>()
}

// Create a new temporary directory to serve as the mock namespace.
pub fn create_tmp_dir(seed_files: Vec<SeedPath>) -> Result<TempDir> {
    let tmp_dir = TempDir::new_in("/tmp")?;
    let tmp_path = tmp_dir.path();

    for seed_path in seed_files {
        match seed_path {
            SeedPath::File(file) => {
                let file_path = tmp_path.join(file.name);
                let dir_path = file_path.parent().unwrap();
                create_dir_all(dir_path)?;
                write(file_path, file.data)?;
            }
            SeedPath::Directory(directory) => {
                let dir_path = tmp_path.join(directory);
                create_dir_all(dir_path)?;
            }
        }
    }

    Ok(tmp_dir)
}
