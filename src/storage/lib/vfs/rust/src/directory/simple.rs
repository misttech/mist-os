// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This is an implementation of "simple" pseudo directories.
//! Use [`crate::directory::immutable::Simple::new()`]
//! to construct actual instances.  See [`Simple`] for details.

use crate::common::CreationMode;
use crate::directory::dirents_sink;
use crate::directory::entry::{DirectoryEntry, EntryInfo, OpenRequest, RequestFlags};
use crate::directory::entry_container::{Directory, DirectoryWatcher};
use crate::directory::helper::{AlreadyExists, DirectlyMutable, NotDirectory};
use crate::directory::immutable::connection::ImmutableConnection;
use crate::directory::traversal_position::TraversalPosition;
use crate::directory::watchers::event_producers::{
    SingleNameEventProducer, StaticVecEventProducer,
};
use crate::directory::watchers::Watchers;
use crate::execution_scope::ExecutionScope;
use crate::name::Name;
use crate::node::Node;
use crate::path::Path;
use crate::protocols::ProtocolsExt;
use crate::{ObjectRequestRef, ToObjectRequest};
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::iter;
use std::sync::{Arc, Mutex};
use zx_status::Status;

use super::entry::GetEntryInfo;

/// An implementation of a "simple" pseudo directory.  This directory holds a set of entries,
/// allowing the server to add or remove entries via the
/// [`crate::directory::helper::DirectlyMutable::add_entry()`] and
/// [`crate::directory::helper::DirectlyMutable::remove_entry`] methods.
pub struct Simple {
    inner: Mutex<Inner>,

    // The inode for this directory. This should either be unique within this VFS, or INO_UNKNOWN.
    inode: u64,

    not_found_handler: Mutex<Option<Box<dyn FnMut(&str) + Send + Sync + 'static>>>,
}

struct Inner {
    entries: BTreeMap<Name, Arc<dyn DirectoryEntry>>,

    watchers: Watchers,
}

impl Simple {
    pub fn new() -> Arc<Self> {
        Self::new_with_inode(fio::INO_UNKNOWN)
    }

    pub(crate) fn new_with_inode(inode: u64) -> Arc<Self> {
        Arc::new(Simple {
            inner: Mutex::new(Inner { entries: BTreeMap::new(), watchers: Watchers::new() }),
            inode,
            not_found_handler: Mutex::new(None),
        })
    }

    /// The provided function will be called whenever this VFS receives an open request for a path
    /// that is not present in the VFS. The function is invoked with the full path of the missing
    /// entry, relative to the root of this VFS. Typically this function is used for logging.
    pub fn set_not_found_handler(
        self: Arc<Self>,
        handler: Box<dyn FnMut(&str) + Send + Sync + 'static>,
    ) {
        let mut this = self.not_found_handler.lock().unwrap();
        this.replace(handler);
    }

    /// Returns the entry identified by `name`.
    pub fn get_entry(&self, name: &str) -> Result<Arc<dyn DirectoryEntry>, Status> {
        crate::name::validate_name(name)?;

        let this = self.inner.lock().unwrap();
        match this.entries.get(name) {
            Some(entry) => Ok(entry.clone()),
            None => Err(Status::NOT_FOUND),
        }
    }

    /// Gets or inserts an entry (as supplied by the callback `f`).
    pub fn get_or_insert<T: DirectoryEntry>(
        &self,
        name: Name,
        f: impl FnOnce() -> Arc<T>,
    ) -> Arc<dyn DirectoryEntry> {
        let mut guard = self.inner.lock().unwrap();
        let inner = &mut *guard;
        match inner.entries.entry(name) {
            Entry::Vacant(slot) => {
                inner.watchers.send_event(&mut SingleNameEventProducer::added(""));
                slot.insert(f()).clone()
            }
            Entry::Occupied(entry) => entry.get().clone(),
        }
    }

    /// Removes all entries from the directory.
    pub fn remove_all_entries(&self) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.entries.is_empty() {
            let names = std::mem::take(&mut inner.entries)
                .into_keys()
                .map(String::from)
                .collect::<Vec<String>>();
            inner.watchers.send_event(&mut StaticVecEventProducer::removed(names));
        }
    }

    fn open_impl<'a, P: ProtocolsExt + ToRequestFlags>(
        self: Arc<Self>,
        scope: ExecutionScope,
        mut path: Path,
        protocols: P,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        // See if the path has a next segment, if so we want to traverse down the directory.
        // Otherwise we've arrived at the right directory.
        let (name, path_ref) = match path.next_with_ref() {
            (path_ref, Some(name)) => (name, path_ref),
            (_, None) => {
                return object_request.spawn_connection(
                    scope,
                    self,
                    protocols,
                    ImmutableConnection::create,
                )
            }
        };

        // Don't hold the inner lock while opening the entry in case the directory contains itself.
        let entry = self.inner.lock().unwrap().entries.get(name).cloned();
        match (entry, path_ref.is_empty(), protocols.creation_mode()) {
            (None, false, _) | (None, true, CreationMode::Never) => {
                // Either:
                //   - we're at an intermediate directory and the next entry doesn't exist, or
                //   - we're at the last directory and the next entry doesn't exist and creating the
                //     entry wasn't requested.
                if let Some(not_found_handler) = &mut *self.not_found_handler.lock().unwrap() {
                    not_found_handler(path_ref.as_str());
                }
                Err(Status::NOT_FOUND)
            }
            (None, true, CreationMode::Always | CreationMode::AllowExisting) => {
                // We're at the last directory and the entry doesn't exist and creating the entry
                // was requested which isn't supported.
                Err(Status::NOT_SUPPORTED)
            }
            (Some(_), true, CreationMode::Always) => {
                // We're at the last directory and the entry exists but creating the entry is
                // required.
                Err(Status::ALREADY_EXISTS)
            }
            (Some(entry), _, _) => entry.open_entry(OpenRequest::new(
                scope,
                protocols.to_request_flags(),
                path,
                object_request,
            )),
        }
    }
}

impl GetEntryInfo for Simple {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(self.inode, fio::DirentType::Directory)
    }
}

impl DirectoryEntry for Simple {
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
        request.open_dir(self)
    }
}

impl Node for Simple {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, Status> {
        Ok(immutable_attributes!(
            requested_attributes,
            Immutable {
                protocols: fio::NodeProtocolKinds::DIRECTORY,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::ENUMERATE
                    | fio::Operations::TRAVERSE,
                id: self.inode,
            }
        ))
    }
}

impl Directory for Simple {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        flags
            .to_object_request(server_end)
            .handle(|object_request| self.open_impl(scope, path, flags, object_request));
    }

    fn open3(
        self: Arc<Self>,
        scope: ExecutionScope,
        path: Path,
        flags: fio::Flags,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        self.open_impl(scope, path, flags, object_request)
    }

    async fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        sink: Box<dyn dirents_sink::Sink>,
    ) -> Result<(TraversalPosition, Box<dyn dirents_sink::Sealed>), Status> {
        use dirents_sink::AppendResult;

        let this = self.inner.lock().unwrap();

        let (mut sink, entries_iter) = match pos {
            TraversalPosition::Start => {
                match sink.append(&EntryInfo::new(self.inode, fio::DirentType::Directory), ".") {
                    AppendResult::Ok(sink) => {
                        // I wonder why, but rustc can not infer T in
                        //
                        //   pub fn range<T, R>(&self, range: R) -> Range<K, V>
                        //   where
                        //     K: Borrow<T>,
                        //     R: RangeBounds<T>,
                        //     T: Ord + ?Sized,
                        //
                        // for some reason here.  It says:
                        //
                        //   error[E0283]: type annotations required: cannot resolve `_: std::cmp::Ord`
                        //
                        // pointing to "range".  Same for two the other "range()" invocations
                        // below.
                        (sink, this.entries.range::<Name, _>(..))
                    }
                    AppendResult::Sealed(sealed) => {
                        let new_pos = match this.entries.keys().next() {
                            None => TraversalPosition::End,
                            Some(first_name) => TraversalPosition::Name(first_name.clone().into()),
                        };
                        return Ok((new_pos, sealed));
                    }
                }
            }

            TraversalPosition::Name(next_name) => {
                // The only way to get a `TraversalPosition::Name` is if we returned it in the
                // `AppendResult::Sealed` code path above. Therefore, the conversion from
                // `next_name` to `Name` will never fail in practice.
                let next: Name = next_name.to_owned().try_into().unwrap();
                (sink, this.entries.range::<Name, _>(next..))
            }

            TraversalPosition::Bytes(_) => unreachable!(),

            TraversalPosition::Index(_) => unreachable!(),

            TraversalPosition::End => return Ok((TraversalPosition::End, sink.seal())),
        };

        for (name, entry) in entries_iter {
            match sink.append(&entry.entry_info(), &name) {
                AppendResult::Ok(new_sink) => sink = new_sink,
                AppendResult::Sealed(sealed) => {
                    return Ok((TraversalPosition::Name(name.clone().into()), sealed));
                }
            }
        }

        Ok((TraversalPosition::End, sink.seal()))
    }

    fn register_watcher(
        self: Arc<Self>,
        scope: ExecutionScope,
        mask: fio::WatchMask,
        watcher: DirectoryWatcher,
    ) -> Result<(), Status> {
        let mut this = self.inner.lock().unwrap();

        let mut names = StaticVecEventProducer::existing({
            let entry_names = this.entries.keys();
            iter::once(".".to_string()).chain(entry_names.map(|x| x.to_owned().into())).collect()
        });

        let controller = this.watchers.add(scope, self.clone(), mask, watcher);
        controller.send_event(&mut names);
        controller.send_event(&mut SingleNameEventProducer::idle());

        Ok(())
    }

    fn unregister_watcher(self: Arc<Self>, key: usize) {
        let mut this = self.inner.lock().unwrap();
        this.watchers.remove(key);
    }
}

impl DirectlyMutable for Simple {
    fn add_entry_impl(
        &self,
        name: Name,
        entry: Arc<dyn DirectoryEntry>,
        overwrite: bool,
    ) -> Result<(), AlreadyExists> {
        let mut this = self.inner.lock().unwrap();

        if !overwrite && this.entries.contains_key(&name) {
            return Err(AlreadyExists);
        }

        this.watchers.send_event(&mut SingleNameEventProducer::added(&name));

        let _ = this.entries.insert(name, entry);
        Ok(())
    }

    fn remove_entry_impl(
        &self,
        name: Name,
        must_be_directory: bool,
    ) -> Result<Option<Arc<dyn DirectoryEntry>>, NotDirectory> {
        let mut this = self.inner.lock().unwrap();

        match this.entries.entry(name) {
            Entry::Vacant(_) => Ok(None),
            Entry::Occupied(occupied) => {
                if must_be_directory
                    && occupied.get().entry_info().type_() != fio::DirentType::Directory
                {
                    Err(NotDirectory)
                } else {
                    let (key, value) = occupied.remove_entry();
                    this.watchers.send_event(&mut SingleNameEventProducer::removed(&key));
                    Ok(Some(value))
                }
            }
        }
    }
}

trait ToRequestFlags {
    fn to_request_flags(&self) -> RequestFlags;
}

impl ToRequestFlags for fio::OpenFlags {
    fn to_request_flags(&self) -> RequestFlags {
        RequestFlags::Open1(*self)
    }
}

impl ToRequestFlags for fio::Flags {
    fn to_request_flags(&self) -> RequestFlags {
        RequestFlags::Open3(*self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::immutable::Simple;
    use crate::{assert_event, file};
    use fidl::endpoints::create_proxy;

    #[test]
    fn add_entry_success() {
        let dir = Simple::new();
        assert_eq!(
            dir.add_entry("path_without_separators", file::read_only(b"test")),
            Ok(()),
            "add entry with valid filename should succeed"
        );
    }

    #[test]
    fn add_entry_error_name_with_path_separator() {
        let dir = Simple::new();
        let status = dir
            .add_entry("path/with/separators", file::read_only(b"test"))
            .expect_err("add entry with path separator should fail");
        assert_eq!(status, Status::INVALID_ARGS);
    }

    #[test]
    fn add_entry_error_name_too_long() {
        let dir = Simple::new();
        let status = dir
            .add_entry("a".repeat(10000), file::read_only(b"test"))
            .expect_err("add entry whose name is too long should fail");
        assert_eq!(status, Status::BAD_PATH);
    }

    #[fuchsia::test]
    async fn not_found_handler() {
        let dir = Simple::new();
        let path_mutex = Arc::new(Mutex::new(None));
        let path_mutex_clone = path_mutex.clone();
        dir.clone().set_not_found_handler(Box::new(move |path| {
            *path_mutex_clone.lock().unwrap() = Some(path.to_string());
        }));

        let sub_dir = Simple::new();
        let path_mutex_clone = path_mutex.clone();
        sub_dir.clone().set_not_found_handler(Box::new(move |path| {
            *path_mutex_clone.lock().unwrap() = Some(path.to_string());
        }));
        dir.add_entry("dir", sub_dir).expect("add entry with valid filename should succeed");

        dir.add_entry("file", file::read_only(b"test"))
            .expect("add entry with valid filename should succeed");

        let scope = ExecutionScope::new();

        for (path, expectation) in vec![
            (".", None),
            ("does-not-exist", Some("does-not-exist".to_string())),
            ("file", None),
            ("dir", None),
            ("dir/does-not-exist", Some("dir/does-not-exist".to_string())),
        ] {
            let (proxy, server_end) = create_proxy::<fio::NodeMarker>();
            let flags = fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::DESCRIBE;
            let path = Path::validate_and_split(path).unwrap();
            dir.clone().open(scope.clone(), flags, path, server_end.into_channel().into());
            assert_event!(proxy, fio::NodeEvent::OnOpen_ { .. }, {});

            assert_eq!(expectation, path_mutex.lock().unwrap().take());
        }
    }

    #[test]
    fn remove_all_entries() {
        let dir = Simple::new();

        dir.add_entry("file", file::read_only(""))
            .expect("add entry with valid filename should succeed");

        dir.remove_all_entries();
        assert_eq!(
            dir.get_entry("file").err().expect("file should no longer exist"),
            Status::NOT_FOUND
        );
    }
}
