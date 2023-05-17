// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use byteorder::{LittleEndian, WriteBytesExt};
use fidl::endpoints::ControlHandle;
use fidl::endpoints::RequestStream;
use fidl::endpoints::ServerEnd;
use fidl::endpoints::{create_endpoints, create_proxy};
use fidl_fuchsia_component_decl::{Component, Expose, ExposeProtocol, ParentRef, Ref, SelfRef};
use fidl_fuchsia_io::{self as fio, DirectoryMarker};
use fidl_fuchsia_sys2 as fsys2;
use fuchsia_async;
use fuchsia_zircon_status::Status;
use futures::{StreamExt, TryStreamExt};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Builder struct for `RealmQueryResult`/
/// This is an builder interface meant to simplify building of test fixtures.
/// Example usage:
/// ```
///   MockRealmQuery.add()
///   .when("other/component") // when client queries for this string ("other/component").
///   .moniker("./other/component") // Returns the following.
///   .exposes(vec![Expose::Protocol(ExposeProtocol {
///       source: Some(Ref::Self_(SelfRef)),
///       target: Some(Ref::Self_(SelfRef)),
///       source_name: Some("src".to_owned()),
///       target_name: Some("fuchsia.io.SomeOtherThing".to_owned()),
///       ..Default::default()
///   })])
///   .add() // Finish building the result.
///   .when("some/thing") // Start another build.
///   ...
/// ```
pub struct MockRealmQueryBuilder {
    mapping: HashMap<String, Box<MockRealmQueryBuilderInner>>,
}

/// Inner struct of `MockRealmQueryBuilder` to provide a builder interface for
/// RealmQuery protocol responses.
pub struct MockRealmQueryBuilderInner {
    when: String,
    moniker: String,
    exposes: Vec<Expose>,
    out_dir_entry: Vec<String>,
    svc_dir_entry: Vec<String>,
    diagnostics_dir_entry: Vec<String>,
    parent: Option<Box<MockRealmQueryBuilder>>,
}

impl MockRealmQueryBuilderInner {
    /// Sets the result moniker.
    pub fn moniker(mut self, moniker: &str) -> Self {
        self.moniker = moniker.to_owned();
        self
    }

    /// Sets the result vector of `Expose`s.
    pub fn exposes(mut self, exposes: Vec<Expose>) -> Self {
        self.exposes = exposes;
        self
    }

    /// Add an entry 'out/'.
    pub fn out_dir_entry(mut self, entry: &str) -> Self {
        self.out_dir_entry.push(entry.to_owned());
        self
    }

    /// Add an entry 'out/svc'.
    pub fn svc_dir_entry(mut self, entry: &str) -> Self {
        self.svc_dir_entry.push(entry.to_owned());
        self
    }

    /// Add an entry `out/diagnostics`.
    pub fn diagnostics_dir_entry(mut self, entry: &str) -> Self {
        self.diagnostics_dir_entry.push(entry.to_owned());
        self
    }

    /// Completes the build and returns a `MockRealmQueryBuilder`.
    pub fn add(mut self) -> MockRealmQueryBuilder {
        let mut parent = *self.parent.unwrap();
        self.parent = None;

        parent.mapping.insert(self.when.to_owned(), Box::new(self));
        parent
    }

    pub fn serve_out_dir(&self, server_end: ServerEnd<DirectoryMarker>) {
        let mut mock_dir_top = MockDir::new("out".to_owned());
        let mut mock_dir_diagnostics = MockDir::new("diagnostics".to_owned());
        let mut mock_dir_svc = MockDir::new("svc".to_owned());

        for entry in &self.diagnostics_dir_entry {
            mock_dir_diagnostics =
                mock_dir_diagnostics.add_entry(MockFile::new_arc(entry.to_owned()));
        }

        for entry in &self.svc_dir_entry {
            mock_dir_svc = mock_dir_svc.add_entry(MockFile::new_arc(entry.to_owned()));
        }

        for entry in &self.out_dir_entry {
            mock_dir_top = mock_dir_top.add_entry(MockFile::new_arc(entry.to_owned()));
        }

        mock_dir_top = mock_dir_top
            .add_entry(Arc::new(mock_dir_diagnostics))
            .add_entry(Arc::new(mock_dir_svc));

        fuchsia_async::Task::local(async move { Arc::new(mock_dir_top).serve(server_end).await })
            .detach();
    }

    fn to_instance(&self) -> fsys2::Instance {
        fsys2::Instance {
            moniker: Some(self.moniker.to_owned()),
            url: Some("".to_owned()),
            instance_id: None,
            resolved_info: Some(fsys2::ResolvedInfo {
                resolved_url: Some("".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn to_manifest(&self) -> Component {
        Component { exposes: Some(self.exposes.clone()), ..Default::default() }
    }
}

impl MockRealmQueryBuilder {
    /// Create a new empty `MockRealmQueryBuilder`.
    pub fn new() -> Self {
        MockRealmQueryBuilder { mapping: HashMap::new() }
    }

    /// Start a build of `RealmQueryResult` by specifying the
    /// expected query string.
    pub fn when(self, at: &str) -> MockRealmQueryBuilderInner {
        MockRealmQueryBuilderInner {
            when: at.to_owned(),
            moniker: "".to_owned(),
            exposes: vec![],
            out_dir_entry: vec![],
            svc_dir_entry: vec![],
            diagnostics_dir_entry: vec![],
            parent: Some(Box::new(self)),
        }
    }

    /// Finish the build and return servable `MockRealmQuery`.
    pub fn build(self) -> MockRealmQuery {
        MockRealmQuery { mapping: self.mapping }
    }
}

/// Provides a mock `RealmQuery` interface.
pub struct MockRealmQuery {
    /// Mapping from Moniker -> Expose.
    mapping: HashMap<String, Box<MockRealmQueryBuilderInner>>,
}

/// Creates the default test fixures for `MockRealmQuery`.
impl Default for MockRealmQuery {
    fn default() -> Self {
        MockRealmQueryBuilder::new()
            .when("example/component")
            .moniker("./example/component")
            .exposes(vec![Expose::Protocol(ExposeProtocol {
                source: Some(Ref::Self_(SelfRef)),
                target: Some(Ref::Parent(ParentRef)),
                source_name: Some("src".to_owned()),
                target_name: Some("fuchsia.diagnostics.ArchiveAccessor".to_owned()),
                ..Default::default()
            })])
            .svc_dir_entry("fuchsia.some.GarbageAccessor")
            .out_dir_entry("fuchsia.diagnostics.WorkingAccessor")
            .diagnostics_dir_entry("fuchsia.inspect.Tree")
            .add()
            .when("other/component")
            .moniker("./other/component")
            .exposes(vec![Expose::Protocol(ExposeProtocol {
                source: Some(Ref::Self_(SelfRef)),
                target: Some(Ref::Parent(ParentRef)),
                source_name: Some("src".to_owned()),
                target_name: Some("fuchsia.io.SomeOtherThing".to_owned()),
                ..Default::default()
            })])
            .add()
            .when("other/component")
            .moniker("./other/component")
            .exposes(vec![Expose::Protocol(ExposeProtocol {
                source: Some(Ref::Self_(SelfRef)),
                target: Some(Ref::Parent(ParentRef)),
                source_name: Some("src".to_owned()),
                target_name: Some("fuchsia.io.MagicStuff".to_owned()),
                ..Default::default()
            })])
            .svc_dir_entry("fuchsia.diagnostics.MagicArchiveAccessor")
            .diagnostics_dir_entry("fuchsia.inspect.Tree")
            .add()
            .when("foo/component")
            .moniker("./foo/component")
            .exposes(vec![Expose::Protocol(ExposeProtocol {
                source: Some(Ref::Self_(SelfRef)),
                target: Some(Ref::Parent(ParentRef)),
                source_name: Some("src".to_owned()),
                target_name: Some("fuchsia.diagnostics.FeedbackArchiveAccessor".to_owned()),
                ..Default::default()
            })])
            .add()
            .when("foo/bar/thing")
            .moniker("./foo/bar/thing")
            .exposes(vec![Expose::Protocol(ExposeProtocol {
                source: Some(Ref::Self_(SelfRef)),
                target: Some(Ref::Parent(ParentRef)),
                source_name: Some("src".to_owned()),
                target_name: Some("fuchsia.diagnostics.FeedbackArchiveAccessor".to_owned()),
                ..Default::default()
            })])
            .add()
            .build()
    }
}

impl MockRealmQuery {
    /// Serves the `RealmQuery` interface asynchronously and runs until the program terminates.
    pub async fn serve(self: Arc<Self>, object: ServerEnd<fsys2::RealmQueryMarker>) {
        let mut stream = object.into_stream().unwrap();
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsys2::RealmQueryRequest::GetInstance { moniker, responder } => {
                    assert!(moniker.starts_with("./"));
                    let query_moniker = &moniker[2..];
                    let res = self.mapping.get(query_moniker).unwrap();
                    responder.send(&mut Ok(res.to_instance())).unwrap();
                }
                fsys2::RealmQueryRequest::Open { moniker, dir_type, object, responder, .. } => {
                    assert!(moniker.starts_with("./"));
                    let query_moniker = &moniker[2..];
                    if let Some(res) = self.mapping.get(query_moniker) {
                        if dir_type == fsys2::OpenDirType::OutgoingDir {
                            // Serve the out dir, everything else doesn't get served.
                            res.serve_out_dir(object.into_channel().into());
                        }
                        responder.send(&mut Ok(())).unwrap();
                    } else {
                        responder.send(&mut Err(fsys2::OpenError::InstanceNotFound)).unwrap();
                    }
                }
                fsys2::RealmQueryRequest::GetManifest { moniker, responder, .. } => {
                    assert!(moniker.starts_with("./"));
                    let query_moniker = &moniker[2..];
                    let res = self.mapping.get(query_moniker).unwrap();
                    let manifest = res.to_manifest();
                    let manifest = fidl::persist(&manifest).unwrap();
                    let (client_end, server_end) =
                        create_endpoints::<fsys2::ManifestBytesIteratorMarker>();

                    fuchsia_async::Task::spawn(async move {
                        let mut stream = server_end.into_stream().unwrap();
                        let fsys2::ManifestBytesIteratorRequest::Next { responder } =
                            stream.next().await.unwrap().unwrap();
                        responder.send(manifest.as_slice()).unwrap();
                        let fsys2::ManifestBytesIteratorRequest::Next { responder } =
                            stream.next().await.unwrap().unwrap();
                        responder.send(&[]).unwrap();
                    })
                    .detach();

                    responder.send(Ok(client_end)).unwrap();
                }
                fsys2::RealmQueryRequest::GetAllInstances { responder } => {
                    let instances: Vec<fsys2::Instance> =
                        self.mapping.values().map(|m| m.to_instance()).collect();

                    let (client_end, server_end) =
                        create_endpoints::<fsys2::InstanceIteratorMarker>();

                    fuchsia_async::Task::spawn(async move {
                        let mut stream = server_end.into_stream().unwrap();
                        let fsys2::InstanceIteratorRequest::Next { responder } =
                            stream.next().await.unwrap().unwrap();
                        responder.send(&instances).unwrap();
                        let fsys2::InstanceIteratorRequest::Next { responder } =
                            stream.next().await.unwrap().unwrap();
                        responder.send(&[]).unwrap();
                    })
                    .detach();

                    responder.send(Ok(client_end)).unwrap();
                }
                _ => unreachable!("request {:?}", request),
            }
        }
    }

    /// Serves the `RealmQuery` interface asynchronously and runs until the program terminates.
    /// Then, instead of needing the client to discover the protocol, return the proxy for futher
    /// test use.
    pub async fn get_proxy(self: Arc<Self>) -> fsys2::RealmQueryProxy {
        let (proxy, server_end) = create_proxy::<fsys2::RealmQueryMarker>().unwrap();
        fuchsia_async::Task::local(async move { self.serve(server_end).await }).detach();
        proxy
    }
}

// Mock directory structure.
pub trait Entry {
    fn open(self: Arc<Self>, flags: fio::OpenFlags, path: &str, object: ServerEnd<fio::NodeMarker>);
    fn encode(&self, buf: &mut Vec<u8>);
    fn name(&self) -> String;
}

pub struct MockDir {
    subdirs: HashMap<String, Arc<dyn Entry>>,
    name: String,
    at_end: AtomicBool,
}

impl MockDir {
    pub fn new(name: String) -> Self {
        MockDir { name, subdirs: HashMap::new(), at_end: AtomicBool::new(false) }
    }

    pub fn new_arc(name: String) -> Arc<Self> {
        Arc::new(Self::new(name))
    }

    pub fn add_entry(mut self, entry: Arc<dyn Entry>) -> Self {
        self.subdirs.insert(entry.name(), entry);
        self
    }

    async fn serve(self: Arc<Self>, object: ServerEnd<fio::DirectoryMarker>) {
        let mut stream = object.into_stream().unwrap();
        let _ = stream.control_handle().send_on_open_(
            Status::OK.into_raw(),
            Some(fio::NodeInfoDeprecated::Directory(fio::DirectoryObject {})),
        );
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fio::DirectoryRequest::Open { flags, mode: _, path, object, .. } => {
                    self.clone().open(flags, &path, object);
                }
                fio::DirectoryRequest::Clone { flags, object, .. } => {
                    self.clone().open(flags | fio::OpenFlags::DIRECTORY, ".", object);
                }
                fio::DirectoryRequest::Rewind { responder, .. } => {
                    self.at_end.store(false, Ordering::Relaxed);
                    responder.send(Status::OK.into_raw()).unwrap();
                }
                fio::DirectoryRequest::ReadDirents { max_bytes: _, responder, .. } => {
                    let entries = match self.at_end.compare_exchange(
                        false,
                        true,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(false) => encode_entries(&self.subdirs),
                        Err(true) => Vec::new(),
                        _ => unreachable!(),
                    };
                    responder.send(Status::OK.into_raw(), &entries).unwrap();
                }
                x => panic!("unsupported request: {:?}", x),
            }
        }
    }
}

fn encode_entries(subdirs: &HashMap<String, Arc<dyn Entry>>) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut data = subdirs.iter().collect::<Vec<(_, _)>>();
    data.sort_by(|a, b| a.0.cmp(b.0));
    for (_, entry) in data.iter() {
        entry.encode(&mut buf);
    }
    buf
}

impl Entry for MockDir {
    fn open(
        self: Arc<Self>,
        flags: fio::OpenFlags,
        path: &str,
        object: ServerEnd<fio::NodeMarker>,
    ) {
        let path = Path::new(path);
        let mut path_iter = path.iter();
        let segment = if let Some(segment) = path_iter.next() {
            if let Some(segment) = segment.to_str() {
                segment
            } else {
                send_error(object, Status::NOT_FOUND);
                return;
            }
        } else {
            "."
        };
        if segment == "." {
            fuchsia_async::Task::local(self.clone().serve(ServerEnd::new(object.into_channel())))
                .detach();
            return;
        }
        if let Some(entry) = self.subdirs.get(segment) {
            entry.clone().open(flags, path_iter.as_path().to_str().unwrap(), object);
        } else {
            send_error(object, Status::NOT_FOUND);
        }
    }

    fn encode(&self, buf: &mut Vec<u8>) {
        buf.write_u64::<LittleEndian>(fio::INO_UNKNOWN).expect("writing mockdir ino to work");
        buf.write_u8(self.name.len() as u8).expect("writing mockdir size to work");
        buf.write_u8(fio::DirentType::Directory.into_primitive())
            .expect("writing mockdir type to work");
        buf.write_all(self.name.as_ref()).expect("writing mockdir name to work");
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}

impl Entry for fio::DirectoryProxy {
    fn open(
        self: Arc<Self>,
        flags: fio::OpenFlags,
        path: &str,
        object: ServerEnd<fio::NodeMarker>,
    ) {
        let _ = fio::DirectoryProxy::open(&*self, flags, fio::ModeType::empty(), path, object);
    }

    fn encode(&self, _buf: &mut Vec<u8>) {
        unimplemented!();
    }

    fn name(&self) -> String {
        unimplemented!();
    }
}

struct MockFile {
    name: String,
}

impl MockFile {
    pub fn new(name: String) -> Self {
        MockFile { name }
    }
    pub fn new_arc(name: String) -> Arc<Self> {
        Arc::new(Self::new(name))
    }
}

impl Entry for MockFile {
    fn open(
        self: Arc<Self>,
        _flags: fio::OpenFlags,
        _path: &str,
        _object: ServerEnd<fio::NodeMarker>,
    ) {
        unimplemented!();
    }

    fn encode(&self, buf: &mut Vec<u8>) {
        buf.write_u64::<LittleEndian>(fio::INO_UNKNOWN).expect("writing mockdir ino to work");
        buf.write_u8(self.name.len() as u8).expect("writing mockdir size to work");
        buf.write_u8(fio::DirentType::File.into_primitive()).expect("writing mockdir type to work");
        buf.write_all(self.name.as_ref()).expect("writing mockdir name to work");
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}

fn send_error(object: ServerEnd<fio::NodeMarker>, status: Status) {
    let stream = object.into_stream().expect("failed to create stream");
    let control_handle = stream.control_handle();
    let _ = control_handle.send_on_open_(status.into_raw(), None);
    control_handle.shutdown_with_epitaph(status);
}
