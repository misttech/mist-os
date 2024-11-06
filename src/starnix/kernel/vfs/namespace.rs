// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mutable_state::{state_accessor, state_implementation};
use crate::task::{CurrentTask, EventHandler, Kernel, Task, WaitCanceler, Waiter};
use crate::time::utc;
use crate::vfs::buffers::InputBuffer;
use crate::vfs::fs_registry::FsRegistry;
use crate::vfs::socket::{SocketAddress, SocketHandle, UnixSocket};
use crate::vfs::{
    fileops_impl_dataless, fileops_impl_delegate_read_and_seek, fileops_impl_nonseekable,
    fileops_impl_noop_sync, fs_node_impl_not_dir, CheckAccessReason, DirEntry, DirEntryHandle,
    DynamicFile, DynamicFileBuf, DynamicFileSource, FileHandle, FileObject, FileOps,
    FileSystemHandle, FileSystemOptions, FsNode, FsNodeHandle, FsNodeOps, FsStr, FsString,
    PathBuilder, RenameFlags, SimpleFileNode, SymlinkTarget, UnlinkKind,
};
use macro_rules_attribute::apply;
use ref_cast::RefCast;
use starnix_logging::log_warn;
use starnix_sync::{
    BeforeFsNodeAppend, FileOpsCore, LockBefore, LockEqualOrBefore, Locked, Mutex, RwLock, Unlocked,
};
use starnix_types::ownership::WeakRef;
use starnix_uapi::arc_key::{ArcKey, PtrKey, WeakKey};
use starnix_uapi::auth::UserAndOrGroupId;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{Access, AccessCheck, FileMode};
use starnix_uapi::inotify_mask::InotifyMask;
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::unmount_flags::UnmountFlags;
use starnix_uapi::vfs::{FdEvents, ResolveFlags};
use starnix_uapi::{errno, error, NAME_MAX};
use std::borrow::Borrow;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Weak};

/// A mount namespace.
///
/// The namespace records at which entries filesystems are mounted.
#[derive(Debug)]
pub struct Namespace {
    root_mount: MountHandle,

    // Unique ID of this namespace.
    pub id: u64,
}

impl Namespace {
    pub fn new(fs: FileSystemHandle) -> Arc<Namespace> {
        Self::new_with_flags(fs, MountFlags::empty())
    }

    pub fn new_with_flags(fs: FileSystemHandle, flags: MountFlags) -> Arc<Namespace> {
        let kernel = fs.kernel.upgrade().expect("can't create namespace without a kernel");
        let root_mount = Mount::new(WhatToMount::Fs(fs), flags);
        Arc::new(Self { root_mount, id: kernel.get_next_namespace_id() })
    }

    pub fn root(&self) -> NamespaceNode {
        self.root_mount.root()
    }

    pub fn clone_namespace(&self) -> Arc<Namespace> {
        let kernel =
            self.root_mount.fs.kernel.upgrade().expect("can't clone namespace without a kernel");
        Arc::new(Self {
            root_mount: self.root_mount.clone_mount_recursive(),
            id: kernel.get_next_namespace_id(),
        })
    }

    /// Assuming new_ns is a clone of the namespace that node is from, return the equivalent of
    /// node in new_ns. If this assumption is violated, returns None.
    pub fn translate_node(mut node: NamespaceNode, new_ns: &Namespace) -> Option<NamespaceNode> {
        // Collect the list of mountpoints that leads to this node's mount
        let mut mountpoints = vec![];
        let mut mount = node.mount;
        while let Some(mountpoint) = mount.as_ref().and_then(|m| m.mountpoint()) {
            mountpoints.push(mountpoint.entry);
            mount = mountpoint.mount;
        }

        // Follow the same path in the new namespace
        let mut mount = Arc::clone(&new_ns.root_mount);
        for mountpoint in mountpoints.iter().rev() {
            let next_mount =
                mount.read().submounts.get(ArcKey::ref_cast(mountpoint))?.mount.clone();
            mount = next_mount;
        }
        node.mount = Some(mount).into();
        Some(node)
    }
}

impl FsNodeOps for Arc<Namespace> {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(MountNamespaceFile(self.clone())))
    }
}

pub struct MountNamespaceFile(pub Arc<Namespace>);

impl FileOps for MountNamespaceFile {
    fileops_impl_nonseekable!();
    fileops_impl_dataless!();
    fileops_impl_noop_sync!();
}

/// An empty struct that we use to track the number of active clients for a mount.
///
/// Each active client takes a reference to this object. The unmount operation fails
/// if there are any active clients of the mount.
type MountClientMarker = Arc<()>;

/// An instance of a filesystem mounted in a namespace.
///
/// At a mount, path traversal switches from one filesystem to another.
/// The client sees a composed directory structure that glues together the
/// directories from the underlying FsNodes from those filesystems.
///
/// The mounts in a namespace form a mount tree, with `mountpoint` pointing to the parent and
/// `submounts` pointing to the children.
pub struct Mount {
    root: DirEntryHandle,
    flags: Mutex<MountFlags>,
    fs: FileSystemHandle,

    /// A unique identifier for this mount reported in /proc/pid/mountinfo.
    id: u64,

    /// A count of the number of active clients.
    active_client_counter: MountClientMarker,

    // Lock ordering: mount -> submount
    state: RwLock<MountState>,
    // Mount used to contain a Weak<Namespace>. It no longer does because since the mount point
    // hash was moved from Namespace to Mount, nothing actually uses it. Now that
    // Namespace::clone_namespace() is implemented in terms of Mount::clone_mount_recursive, it
    // won't be trivial to add it back. I recommend turning the mountpoint field into an enum of
    // Mountpoint or Namespace, maybe called "parent", and then traverse up to the top of the tree
    // if you need to find a Mount's Namespace.
}
type MountHandle = Arc<Mount>;

/// Public representation of the mount options.
#[derive(Clone, Debug)]
pub struct MountInfo {
    handle: Option<MountHandle>,
}

impl MountInfo {
    /// `MountInfo` for a element that is not tied to a given mount. Mount flags will be considered
    /// empty.
    pub fn detached() -> Self {
        None.into()
    }

    /// The mount flags of the represented mount.
    pub fn flags(&self) -> MountFlags {
        if let Some(handle) = &self.handle {
            handle.flags()
        } else {
            // Consider not mounted node have the NOATIME flags.
            MountFlags::NOATIME
        }
    }

    /// Checks whether this `MountInfo` represents a writable file system mount.
    pub fn check_readonly_filesystem(&self) -> Result<(), Errno> {
        if self.flags().contains(MountFlags::RDONLY) {
            return error!(EROFS);
        }
        Ok(())
    }

    /// Checks whether this `MountInfo` represents an executable file system mount.
    pub fn check_noexec_filesystem(&self) -> Result<(), Errno> {
        if self.flags().contains(MountFlags::NOEXEC) {
            return error!(EACCES);
        }
        Ok(())
    }
}

impl Deref for MountInfo {
    type Target = Option<MountHandle>;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl DerefMut for MountInfo {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.handle
    }
}

impl std::cmp::PartialEq for MountInfo {
    fn eq(&self, other: &Self) -> bool {
        self.handle.as_ref().map(Arc::as_ptr) == other.handle.as_ref().map(Arc::as_ptr)
    }
}

impl std::cmp::Eq for MountInfo {}

impl Into<MountInfo> for Option<MountHandle> {
    fn into(self) -> MountInfo {
        MountInfo { handle: self }
    }
}

#[derive(Default)]
pub struct MountState {
    /// The namespace node that this mount is mounted on. This is a tuple instead of a
    /// NamespaceNode because the Mount pointer has to be weak because this is the pointer to the
    /// parent mount, the parent has a pointer to the children too, and making both strong would be
    /// a cycle.
    mountpoint: Option<(Weak<Mount>, DirEntryHandle)>,

    // The set is keyed by the mountpoints which are always descendants of this mount's root.
    // Conceptually, the set is more akin to a map: `DirEntry -> MountHandle`, but we use a set
    // instead because `Submount` has a drop implementation that needs both the key and value.
    //
    // Each directory entry can only have one mount attached. Mount shadowing works by using the
    // root of the inner mount as a mountpoint. For example, if filesystem A is mounted at /foo,
    // mounting filesystem B on /foo will create the mount as a child of the A mount, attached to
    // A's root, instead of the root mount.
    submounts: HashSet<Submount>,

    /// The membership of this mount in its peer group. Do not access directly. Instead use
    /// peer_group(), take_from_peer_group(), and set_peer_group().
    // TODO(tbodt): Refactor the links into, some kind of extra struct or something? This is hard
    // because setting this field requires the Arc<Mount>.
    peer_group_: Option<(Arc<PeerGroup>, PtrKey<Mount>)>,
    /// The membership of this mount in a PeerGroup's downstream. Do not access directly. Instead
    /// use upstream(), take_from_upstream(), and set_upstream().
    upstream_: Option<(Weak<PeerGroup>, PtrKey<Mount>)>,
}

/// A group of mounts. Setting MS_SHARED on a mount puts it in its own peer group. Any bind mounts
/// of a mount in the group are also added to the group. A mount created in any mount in a peer
/// group will be automatically propagated (recreated) in every other mount in the group.
#[derive(Default)]
struct PeerGroup {
    id: u64,
    state: RwLock<PeerGroupState>,
}
#[derive(Default)]
struct PeerGroupState {
    mounts: HashSet<WeakKey<Mount>>,
    downstream: HashSet<WeakKey<Mount>>,
}

pub enum WhatToMount {
    Fs(FileSystemHandle),
    Bind(NamespaceNode),
}

impl Mount {
    pub fn new(what: WhatToMount, flags: MountFlags) -> MountHandle {
        match what {
            WhatToMount::Fs(fs) => Self::new_with_root(fs.root().clone(), flags),
            WhatToMount::Bind(node) => {
                let mount = node.mount.as_ref().expect("can't bind mount from an anonymous node");
                mount.clone_mount(&node.entry, flags)
            }
        }
    }

    fn new_with_root(root: DirEntryHandle, flags: MountFlags) -> MountHandle {
        let known_flags = MountFlags::STORED_ON_MOUNT;
        assert!(
            !flags.intersects(!known_flags),
            "mount created with extra flags {:?}",
            flags - known_flags
        );
        let fs = root.node.fs();
        let kernel = fs.kernel.upgrade().expect("can't create mount without kernel");
        Arc::new(Self {
            id: kernel.get_next_mount_id(),
            flags: Mutex::new(flags),
            root,
            active_client_counter: Default::default(),
            fs,
            state: Default::default(),
        })
    }

    /// A namespace node referring to the root of the mount.
    pub fn root(self: &MountHandle) -> NamespaceNode {
        NamespaceNode::new(Arc::clone(self), Arc::clone(&self.root))
    }

    /// Returns true if there is a submount on top of `dir_entry`.
    pub fn has_submount(&self, dir_entry: &DirEntryHandle) -> bool {
        self.state.read().submounts.contains(ArcKey::ref_cast(dir_entry))
    }

    /// The NamespaceNode on which this Mount is mounted.
    fn mountpoint(&self) -> Option<NamespaceNode> {
        let state = self.state.read();
        let (ref mount, ref entry) = state.mountpoint.as_ref()?;
        Some(NamespaceNode::new(mount.upgrade()?, entry.clone()))
    }

    /// Create the specified mount as a child. Also propagate it to the mount's peer group.
    fn create_submount(
        self: &MountHandle,
        dir: &DirEntryHandle,
        what: WhatToMount,
        flags: MountFlags,
    ) {
        // TODO(tbodt): Making a copy here is necessary for lock ordering, because the peer group
        // lock nests inside all mount locks (it would be impractical to reverse this because you
        // need to lock a mount to get its peer group.) But it opens the door to race conditions
        // where if a peer are concurrently being added, the mount might not get propagated to the
        // new peer. The only true solution to this is bigger locks, somehow using the same lock
        // for the peer group and all of the mounts in the group. Since peer groups are fluid and
        // can have mounts constantly joining and leaving and then joining other groups, the only
        // sensible locking option is to use a single global lock for all mounts and peer groups.
        // This is almost impossible to express in rust. Help.
        //
        // Update: Also necessary to make a copy to prevent excess replication, see the comment on
        // the following Mount::new call.
        let peers = {
            let state = self.state.read();
            state.peer_group().map(|g| g.copy_propagation_targets()).unwrap_or_default()
        };

        // Create the mount after copying the peer groups, because in the case of creating a bind
        // mount inside itself, the new mount would get added to our peer group during the
        // Mount::new call, but we don't want to replicate into it already. For an example see
        // MountTest.QuizBRecursion.
        let mount = Mount::new(what, flags);

        if self.read().is_shared() {
            mount.write().make_shared();
        }

        for peer in peers {
            if Arc::ptr_eq(self, &peer) {
                continue;
            }
            let clone = mount.clone_mount_recursive();
            peer.write().add_submount_internal(dir, clone);
        }

        self.write().add_submount_internal(dir, mount)
    }

    fn remove_submount(
        self: &MountHandle,
        mount_hash_key: &ArcKey<DirEntry>,
        propagate: bool,
    ) -> Result<(), Errno> {
        if propagate {
            // create_submount explains why we need to make a copy of peers.
            let peers = {
                let state = self.state.read();
                state.peer_group().map(|g| g.copy_propagation_targets()).unwrap_or_default()
            };

            for peer in peers {
                if Arc::ptr_eq(self, &peer) {
                    continue;
                }
                let _ = peer.write().remove_submount_internal(mount_hash_key);
            }
        }

        self.write().remove_submount_internal(mount_hash_key)
    }

    /// Create a new mount with the same filesystem, flags, and peer group. Used to implement bind
    /// mounts.
    fn clone_mount(
        self: &MountHandle,
        new_root: &DirEntryHandle,
        flags: MountFlags,
    ) -> MountHandle {
        assert!(new_root.is_descendant_of(&self.root));
        // According to mount(2) on bind mounts, all flags other than MS_REC are ignored when doing
        // a bind mount.
        let clone = Self::new_with_root(Arc::clone(new_root), self.flags());

        if flags.contains(MountFlags::REC) {
            // This is two steps because the alternative (locking clone.state while iterating over
            // self.state.submounts) trips tracing_mutex. The lock ordering is parent -> child, and
            // if the clone is eventually made a child of self, this looks like an ordering
            // violation. I'm not convinced it's a real issue, but I can't convince myself it's not
            // either.
            let mut submounts = vec![];
            for Submount { dir, mount } in &self.state.read().submounts {
                submounts.push((dir.clone(), mount.clone_mount_recursive()));
            }
            let mut clone_state = clone.write();
            for (dir, submount) in submounts {
                clone_state.add_submount_internal(&dir, submount);
            }
        }

        // Put the clone in the same peer group
        let peer_group = self.state.read().peer_group().map(Arc::clone);
        if let Some(peer_group) = peer_group {
            clone.write().set_peer_group(peer_group);
        }

        clone
    }

    /// Do a clone of the full mount hierarchy below this mount. Used for creating mount
    /// namespaces and creating copies to use for propagation.
    fn clone_mount_recursive(self: &MountHandle) -> MountHandle {
        self.clone_mount(&self.root, MountFlags::REC)
    }

    pub fn change_propagation(self: &MountHandle, flag: MountFlags, recursive: bool) {
        let mut state = self.write();
        match flag {
            MountFlags::SHARED => state.make_shared(),
            MountFlags::PRIVATE => state.make_private(),
            MountFlags::DOWNSTREAM => state.make_downstream(),
            _ => {
                log_warn!("mount propagation {:?}", flag);
                return;
            }
        }

        if recursive {
            for submount in &state.submounts {
                submount.mount.change_propagation(flag, recursive);
            }
        }
    }

    fn flags(self: &MountHandle) -> MountFlags {
        *self.flags.lock()
    }

    pub fn update_flags(self: &MountHandle, mut flags: MountFlags) {
        flags &= MountFlags::STORED_ON_MOUNT;
        let atime_flags = MountFlags::NOATIME
            | MountFlags::NODIRATIME
            | MountFlags::RELATIME
            | MountFlags::STRICTATIME;
        let mut stored_flags = self.flags.lock();
        if !flags.intersects(atime_flags) {
            // Since Linux 3.17, if none of MS_NOATIME, MS_NODIRATIME,
            // MS_RELATIME, or MS_STRICTATIME is specified in mountflags, then
            // the remount operation preserves the existing values of these
            // flags (rather than defaulting to MS_RELATIME).
            flags |= *stored_flags & atime_flags;
        }
        // The "effect [of MS_STRICTATIME] is to clear the MS_NOATIME and MS_RELATIME flags."
        flags &= !MountFlags::STRICTATIME;
        *stored_flags = flags;
    }

    /// The number of active clients of this mount.
    ///
    /// The mount cannot be unmounted if there are any active clients.
    fn active_clients(&self) -> usize {
        // We need to subtract one for our own reference. We are not a real client.
        Arc::strong_count(&self.active_client_counter) - 1
    }

    pub fn unmount(&self, flags: UnmountFlags, propagate: bool) -> Result<(), Errno> {
        if !flags.contains(UnmountFlags::DETACH) {
            if self.active_clients() > 0 || !self.state.read().submounts.is_empty() {
                return error!(EBUSY);
            }
        }
        let mountpoint = self.mountpoint().ok_or_else(|| errno!(EINVAL))?;
        let parent_mount = mountpoint.mount.as_ref().expect("a mountpoint must be part of a mount");
        parent_mount.remove_submount(mountpoint.mount_hash_key(), propagate)
    }

    state_accessor!(Mount, state, Arc<Mount>);
}

impl MountState {
    /// Return this mount's current peer group.
    fn peer_group(&self) -> Option<&Arc<PeerGroup>> {
        let (ref group, _) = self.peer_group_.as_ref()?;
        Some(group)
    }

    /// Remove this mount from its peer group and return the peer group.
    fn take_from_peer_group(&mut self) -> Option<Arc<PeerGroup>> {
        let (old_group, old_mount) = self.peer_group_.take()?;
        old_group.remove(old_mount);
        if let Some(upstream) = self.take_from_upstream() {
            let next_mount =
                old_group.state.read().mounts.iter().next().map(|w| w.0.upgrade().unwrap());
            if let Some(next_mount) = next_mount {
                // TODO(https://fxbug.dev/42065259): Fix the lock ordering here. We've locked next_mount
                // while self is locked, and since the propagation tree and mount tree are
                // separate, this could violate the mount -> submount order previously established.
                next_mount.write().set_upstream(upstream);
            }
        }
        Some(old_group)
    }

    fn upstream(&self) -> Option<Arc<PeerGroup>> {
        self.upstream_.as_ref().and_then(|g| g.0.upgrade())
    }

    fn take_from_upstream(&mut self) -> Option<Arc<PeerGroup>> {
        let (old_upstream, old_mount) = self.upstream_.take()?;
        // TODO(tbodt): Reason about whether the upgrade() could possibly return None, and what we
        // should actually do in that case.
        let old_upstream = old_upstream.upgrade()?;
        old_upstream.remove_downstream(old_mount);
        Some(old_upstream)
    }
}

#[apply(state_implementation!)]
impl MountState<Base = Mount, BaseType = Arc<Mount>> {
    /// Add a child mount *without propagating it to the peer group*. For internal use only.
    fn add_submount_internal(&mut self, dir: &DirEntryHandle, mount: MountHandle) {
        if !dir.is_descendant_of(&self.base.root) {
            return;
        }

        let submount = mount.fs.kernel.upgrade().unwrap().mounts.register_mount(dir, mount.clone());
        let old_mountpoint =
            mount.state.write().mountpoint.replace((Arc::downgrade(self.base), Arc::clone(dir)));
        assert!(old_mountpoint.is_none(), "add_submount can only take a newly created mount");
        // Mount shadowing is implemented by mounting onto the root of the first mount, not by
        // creating two mounts on the same mountpoint.
        let old_mount = self.submounts.replace(submount);

        // In rare cases, mount propagation might result in a request to mount on a directory where
        // something is already mounted. MountTest.LotsOfShadowing will trigger this. Linux handles
        // this by inserting the new mount between the old mount and the current mount.
        if let Some(mut old_mount) = old_mount {
            // Previous state: self[dir] = old_mount
            // New state: self[dir] = new_mount, new_mount[new_mount.root] = old_mount
            // The new mount has already been inserted into self, now just update the old mount to
            // be a child of the new mount.
            old_mount.mount.write().mountpoint = Some((Arc::downgrade(&mount), Arc::clone(dir)));
            old_mount.dir = ArcKey(mount.root.clone());
            mount.write().submounts.insert(old_mount);
        }
    }

    fn remove_submount_internal(&mut self, mount_hash_key: &ArcKey<DirEntry>) -> Result<(), Errno> {
        if self.submounts.remove(mount_hash_key) {
            Ok(())
        } else {
            error!(EINVAL)
        }
    }

    /// Set this mount's peer group.
    fn set_peer_group(&mut self, group: Arc<PeerGroup>) {
        self.take_from_peer_group();
        group.add(self.base);
        self.peer_group_ = Some((group, Arc::as_ptr(self.base).into()));
    }

    fn set_upstream(&mut self, group: Arc<PeerGroup>) {
        self.take_from_upstream();
        group.add_downstream(self.base);
        self.upstream_ = Some((Arc::downgrade(&group), Arc::as_ptr(self.base).into()));
    }

    /// Is the mount in a peer group? Corresponds to MS_SHARED.
    pub fn is_shared(&self) -> bool {
        self.peer_group().is_some()
    }

    /// Put the mount in a peer group. Implements MS_SHARED.
    pub fn make_shared(&mut self) {
        if self.is_shared() {
            return;
        }
        let kernel =
            self.base.fs.kernel.upgrade().expect("can't create new peer group without kernel");
        self.set_peer_group(PeerGroup::new(kernel.get_next_peer_group_id()));
    }

    /// Take the mount out of its peer group, also remove upstream if any. Implements MS_PRIVATE.
    pub fn make_private(&mut self) {
        self.take_from_peer_group();
        self.take_from_upstream();
    }

    /// Take the mount out of its peer group and make it downstream instead. Implements
    /// MountFlags::DOWNSTREAM (MS_SLAVE).
    pub fn make_downstream(&mut self) {
        if let Some(peer_group) = self.take_from_peer_group() {
            self.set_upstream(peer_group);
        }
    }
}

impl PeerGroup {
    fn new(id: u64) -> Arc<Self> {
        Arc::new(Self { id, state: Default::default() })
    }

    fn add(&self, mount: &Arc<Mount>) {
        self.state.write().mounts.insert(WeakKey::from(mount));
    }

    fn remove(&self, mount: PtrKey<Mount>) {
        self.state.write().mounts.remove(&mount);
    }

    fn add_downstream(&self, mount: &Arc<Mount>) {
        self.state.write().downstream.insert(WeakKey::from(mount));
    }

    fn remove_downstream(&self, mount: PtrKey<Mount>) {
        self.state.write().downstream.remove(&mount);
    }

    fn copy_propagation_targets(&self) -> Vec<MountHandle> {
        let mut buf = vec![];
        self.collect_propagation_targets(&mut buf);
        buf
    }

    fn collect_propagation_targets(&self, buf: &mut Vec<MountHandle>) {
        let downstream_mounts: Vec<_> = {
            let state = self.state.read();
            buf.extend(state.mounts.iter().filter_map(|m| m.0.upgrade()));
            state.downstream.iter().filter_map(|m| m.0.upgrade()).collect()
        };
        for mount in downstream_mounts {
            let peer_group = mount.read().peer_group().map(Arc::clone);
            match peer_group {
                Some(group) => group.collect_propagation_targets(buf),
                None => buf.push(mount),
            }
        }
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        state.take_from_peer_group();
        state.take_from_upstream();
    }
}

impl fmt::Debug for Mount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.read();
        f.debug_struct("Mount")
            .field("id", &(self as *const Mount))
            .field("root", &self.root)
            .field("mountpoint", &state.mountpoint)
            .field("submounts", &state.submounts)
            .finish()
    }
}

impl Kernel {
    pub fn get_next_mount_id(&self) -> u64 {
        self.next_mount_id.next()
    }

    pub fn get_next_peer_group_id(&self) -> u64 {
        self.next_peer_group_id.next()
    }

    pub fn get_next_namespace_id(&self) -> u64 {
        self.next_namespace_id.next()
    }
}

impl CurrentTask {
    pub fn create_filesystem(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        fs_type: &FsStr,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        // Please register new file systems via //src/starnix/modules/lib.rs, even if the file
        // system is implemented inside starnix_core.
        //
        // Most file systems should be implemented as modules. The VFS provides various traits that
        // let starnix_core integrate file systems without needing to depend on the file systems
        // directly.
        self.kernel()
            .expando
            .get::<FsRegistry>()
            .create(locked, self, fs_type, options)
            .ok_or_else(|| errno!(ENODEV, fs_type))?
    }
}

struct ProcMountsFileSource(WeakRef<Task>);

impl DynamicFileSource for ProcMountsFileSource {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        // TODO(tbodt): We should figure out a way to have a real iterator instead of grabbing the
        // entire list in one go. Should we have a BTreeMap<u64, Weak<Mount>> in the Namespace?
        // Also has the benefit of correct (i.e. chronological) ordering. But then we have to do
        // extra work to maintain it.
        let task = Task::from_weak(&self.0)?;
        let root = task.fs().root();
        let ns = task.fs().namespace();
        for_each_mount(&ns.root_mount, &mut |mount| {
            let mountpoint = mount.mountpoint().unwrap_or_else(|| mount.root());
            if !mountpoint.is_descendant_of(&root) {
                return Ok(());
            }
            writeln!(
                sink,
                "{} {} {} {} 0 0",
                mount.fs.options.source_for_display(),
                mountpoint.path(&task),
                mount.fs.name(),
                mount.flags(),
            )?;
            Ok(())
        })?;
        Ok(())
    }
}

pub struct ProcMountsFile {
    dynamic_file: DynamicFile<ProcMountsFileSource>,
}

impl ProcMountsFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        SimpleFileNode::new(move || {
            Ok(Self { dynamic_file: DynamicFile::new(ProcMountsFileSource(task.clone())) })
        })
    }
}

impl FileOps for ProcMountsFile {
    fileops_impl_delegate_read_and_seek!(self, self.dynamic_file);
    fileops_impl_noop_sync!();

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSYS)
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        _events: FdEvents,
        _handler: EventHandler,
    ) -> Option<WaitCanceler> {
        // Polling this file gives notifications when any change to mounts occurs. This is not
        // implemented yet, but stubbed for Android init.
        Some(waiter.fake_wait())
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(FdEvents::empty())
    }
}

#[derive(Clone)]
pub struct ProcMountinfoFile(WeakRef<Task>);
impl ProcMountinfoFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task))
    }
}
impl DynamicFileSource for ProcMountinfoFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        // Returns path to the `dir` from the root of the file system.
        fn path_from_fs_root(dir: &DirEntryHandle) -> FsString {
            let mut path = PathBuilder::new();
            if dir.is_dead() {
                // Return `/foo/dir//deleted` if the dir was deleted.
                path.prepend_element("/deleted".into());
            }
            let mut current = dir.clone();
            while let Some(next) = current.parent() {
                path.prepend_element(current.local_name().as_ref());
                current = next
            }
            path.build_absolute()
        }

        // TODO(tbodt): We should figure out a way to have a real iterator instead of grabbing the
        // entire list in one go. Should we have a BTreeMap<u64, Weak<Mount>> in the Namespace?
        // Also has the benefit of correct (i.e. chronological) ordering. But then we have to do
        // extra work to maintain it.
        let task = Task::from_weak(&self.0)?;
        let root = task.fs().root();
        let ns = task.fs().namespace();
        for_each_mount(&ns.root_mount, &mut |mount| {
            let mountpoint = mount.mountpoint().unwrap_or_else(|| mount.root());
            if !mountpoint.is_descendant_of(&root) {
                return Ok(());
            }
            // Can't fail, mountpoint() and root() can't return a NamespaceNode with no mount
            let parent = mountpoint.mount.as_ref().unwrap();
            write!(
                sink,
                "{} {} {} {} {} {}",
                mount.id,
                parent.id,
                mount.root.node.fs().dev_id,
                path_from_fs_root(&mount.root),
                mountpoint.path(&task),
                mount.flags(),
            )?;
            if let Some(peer_group) = mount.read().peer_group() {
                write!(sink, " shared:{}", peer_group.id)?;
            }
            if let Some(upstream) = mount.read().upstream() {
                write!(sink, " master:{}", upstream.id)?;
            }
            writeln!(
                sink,
                " - {} {} {}",
                mount.fs.name(),
                mount.fs.options.source_for_display(),
                mount.fs.options.flags,
            )?;
            Ok(())
        })?;
        Ok(())
    }
}

fn for_each_mount<E>(
    mount: &MountHandle,
    callback: &mut impl FnMut(&MountHandle) -> Result<(), E>,
) -> Result<(), E> {
    callback(mount)?;
    // Collect list first to avoid self deadlock when ProcMountinfoFile::read_at tries to call
    // NamespaceNode::path()
    let submounts: Vec<_> = mount.read().submounts.iter().map(|s| s.mount.clone()).collect();
    for submount in submounts {
        for_each_mount(&submount, callback)?;
    }
    Ok(())
}

/// The `SymlinkMode` enum encodes how symlinks are followed during path traversal.
#[derive(Default, PartialEq, Eq, Copy, Clone, Debug)]
pub enum SymlinkMode {
    /// Follow a symlink at the end of a path resolution.
    #[default]
    Follow,

    /// Do not follow a symlink at the end of a path resolution.
    NoFollow,
}

/// The maximum number of symlink traversals that can be made during path resolution.
pub const MAX_SYMLINK_FOLLOWS: u8 = 40;

/// The context passed during namespace lookups.
///
/// Namespace lookups need to mutate a shared context in order to correctly
/// count the number of remaining symlink traversals.
pub struct LookupContext {
    /// The SymlinkMode for the lookup.
    ///
    /// As the lookup proceeds, the follow count is decremented each time the
    /// lookup traverses a symlink.
    pub symlink_mode: SymlinkMode,

    /// The number of symlinks remaining the follow.
    ///
    /// Each time path resolution calls readlink, this value is decremented.
    pub remaining_follows: u8,

    /// Whether the result of the lookup must be a directory.
    ///
    /// For example, if the path ends with a `/` or if userspace passes
    /// O_DIRECTORY. This flag can be set to true if the lookup encounters a
    /// symlink that ends with a `/`.
    pub must_be_directory: bool,

    /// Resolve flags passed to `openat2`. Empty if the lookup originated in any other syscall.
    pub resolve_flags: ResolveFlags,

    /// Base directory for the lookup. Set only when either `RESOLVE_BENEATH` or `RESOLVE_IN_ROOT`
    /// is passed to `openat2`.
    pub resolve_base: ResolveBase,
}

/// Used to specify base directory in `LookupContext` for lookups originating in the `openat2`
/// syscall with either `RESOLVE_BENEATH` or `RESOLVE_IN_ROOT` flag.
#[derive(Clone, Eq, PartialEq)]
pub enum ResolveBase {
    None,

    /// The lookup is not allowed to traverse any node that's not beneath the specified node.
    Beneath(NamespaceNode),

    /// The lookup should be handled as if the root specified node is the file-system root.
    InRoot(NamespaceNode),
}

impl LookupContext {
    pub fn new(symlink_mode: SymlinkMode) -> LookupContext {
        LookupContext {
            symlink_mode,
            remaining_follows: MAX_SYMLINK_FOLLOWS,
            must_be_directory: false,
            resolve_flags: ResolveFlags::empty(),
            resolve_base: ResolveBase::None,
        }
    }

    pub fn with(&self, symlink_mode: SymlinkMode) -> LookupContext {
        LookupContext { symlink_mode, resolve_base: self.resolve_base.clone(), ..*self }
    }

    pub fn update_for_path(&mut self, path: &FsStr) {
        if path.last() == Some(&b'/') {
            // The last path element must resolve to a directory. This is because a trailing slash
            // was found in the path.
            self.must_be_directory = true;
            // If the last path element is a symlink, we should follow it.
            // See https://pubs.opengroup.org/onlinepubs/9699919799/xrat/V4_xbd_chap03.html#tag_21_03_00_75
            self.symlink_mode = SymlinkMode::Follow;
        }
    }
}

impl Default for LookupContext {
    fn default() -> Self {
        LookupContext::new(SymlinkMode::Follow)
    }
}

/// Whether the path is reachable from the given root.
pub enum PathWithReachability {
    /// The path is reachable from the given root.
    Reachable(FsString),

    /// The path is not reachable from the given root.
    Unreachable(FsString),
}

impl PathWithReachability {
    pub fn into_path(self) -> FsString {
        match self {
            PathWithReachability::Reachable(path) => path,
            PathWithReachability::Unreachable(path) => path,
        }
    }
}

/// A node in a mount namespace.
///
/// This tree is a composite of the mount tree and the FsNode tree.
///
/// These nodes are used when traversing paths in a namespace in order to
/// present the client the directory structure that includes the mounted
/// filesystems.
#[derive(Clone)]
pub struct NamespaceNode {
    /// The mount where this namespace node is mounted.
    ///
    /// A given FsNode can be mounted in multiple places in a namespace. This
    /// field distinguishes between them.
    pub mount: MountInfo,

    /// The FsNode that corresponds to this namespace entry.
    pub entry: DirEntryHandle,
}

impl NamespaceNode {
    pub fn new(mount: MountHandle, entry: DirEntryHandle) -> Self {
        Self { mount: Some(mount).into(), entry }
    }

    /// Create a namespace node that is not mounted in a namespace.
    pub fn new_anonymous(entry: DirEntryHandle) -> Self {
        Self { mount: None.into(), entry }
    }

    /// Create a namespace node that is not mounted in a namespace and that refers to a node that
    /// is not rooted in a hierarchy and has no name.
    pub fn new_anonymous_unrooted(node: FsNodeHandle) -> Self {
        Self::new_anonymous(DirEntry::new_unrooted(node))
    }

    /// Create a FileObject corresponding to this namespace node.
    ///
    /// This function is the primary way of instantiating FileObjects. Each
    /// FileObject records the NamespaceNode that created it in order to
    /// remember its path in the Namespace.
    pub fn open(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        flags: OpenFlags,
        access_check: AccessCheck,
    ) -> Result<FileHandle, Errno> {
        FileObject::new(
            current_task,
            self.entry.node.open(locked, current_task, &self.mount, flags, access_check)?,
            self.clone(),
            flags,
        )
    }

    /// Create or open a node in the file system.
    ///
    /// Works for any type of node other than a symlink.
    ///
    /// Will return an existing node unless `flags` contains `OpenFlags::EXCL`.
    pub fn open_create_node<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
        flags: OpenFlags,
    ) -> Result<NamespaceNode, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        let owner = current_task.as_fscred();
        let mode = current_task.fs().apply_umask(mode);
        let create_fn =
            |locked: &mut Locked<'_, L>, dir: &FsNodeHandle, mount: &MountInfo, name: &_| {
                dir.mknod(locked, current_task, mount, name, mode, dev, owner)
            };
        let entry = if flags.contains(OpenFlags::EXCL) {
            self.entry.create_entry(locked, current_task, &self.mount, name, create_fn)
        } else {
            self.entry.get_or_create_entry(locked, current_task, &self.mount, name, create_fn)
        }?;
        Ok(self.with_new_entry(entry))
    }

    pub fn into_active(self) -> ActiveNamespaceNode {
        ActiveNamespaceNode::new(self)
    }

    /// Create a node in the file system.
    ///
    /// Works for any type of node other than a symlink.
    ///
    /// Does not return an existing node.
    pub fn create_node<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
    ) -> Result<NamespaceNode, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        let owner = current_task.as_fscred();
        let mode = current_task.fs().apply_umask(mode);
        let entry = self.entry.create_entry(
            locked,
            current_task,
            &self.mount,
            name,
            |locked, dir, mount, name| {
                dir.mknod(locked, current_task, mount, name, mode, dev, owner)
            },
        )?;
        Ok(self.with_new_entry(entry))
    }

    /// Create a symlink in the file system.
    ///
    /// To create another type of node, use `create_node`.
    pub fn create_symlink<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        target: &FsStr,
    ) -> Result<NamespaceNode, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        let owner = current_task.as_fscred();
        let entry = self.entry.create_entry(
            locked,
            current_task,
            &self.mount,
            name,
            |locked, dir, mount, name| {
                dir.create_symlink(locked, current_task, mount, name, target, owner)
            },
        )?;
        Ok(self.with_new_entry(entry))
    }

    /// Creates an anonymous file.
    ///
    /// The FileMode::IFMT of the FileMode is always FileMode::IFREG.
    ///
    /// Used by O_TMPFILE.
    pub fn create_tmpfile<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mode: FileMode,
        flags: OpenFlags,
    ) -> Result<NamespaceNode, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        let owner = current_task.as_fscred();
        let mode = current_task.fs().apply_umask(mode);
        Ok(self.with_new_entry(self.entry.create_tmpfile(
            locked,
            current_task,
            &self.mount,
            mode,
            owner,
            flags,
        )?))
    }

    pub fn link<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<NamespaceNode, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        let dir_entry = self.entry.create_entry(
            locked,
            current_task,
            &self.mount,
            name,
            |locked, dir, mount, name| dir.link(locked, current_task, mount, name, child),
        )?;
        Ok(self.with_new_entry(dir_entry))
    }

    pub fn bind_socket<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        socket: SocketHandle,
        socket_address: SocketAddress,
        mode: FileMode,
    ) -> Result<NamespaceNode, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        let dir_entry = self.entry.create_entry(
            locked,
            current_task,
            &self.mount,
            name,
            |locked, dir, mount, name| {
                let node = dir.mknod(
                    locked,
                    current_task,
                    mount,
                    name,
                    mode,
                    DeviceType::NONE,
                    current_task.as_fscred(),
                )?;
                if let Some(unix_socket) = socket.downcast_socket::<UnixSocket>() {
                    unix_socket.bind_socket_to_node(&socket, socket_address, &node)?;
                } else {
                    return error!(ENOTSUP);
                }
                Ok(node)
            },
        )?;
        Ok(self.with_new_entry(dir_entry))
    }

    pub fn unlink<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        kind: UnlinkKind,
        must_be_directory: bool,
    ) -> Result<(), Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        if DirEntry::is_reserved_name(name) {
            match kind {
                UnlinkKind::Directory => {
                    if name == ".." {
                        error!(ENOTEMPTY)
                    } else if self.parent().is_none() {
                        // The client is attempting to remove the root.
                        error!(EBUSY)
                    } else {
                        error!(EINVAL)
                    }
                }
                UnlinkKind::NonDirectory => error!(ENOTDIR),
            }
        } else {
            self.entry.unlink(locked, current_task, &self.mount, name, kind, must_be_directory)
        }
    }

    /// Traverse down a parent-to-child link in the namespace.
    pub fn lookup_child<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        context: &mut LookupContext,
        basename: &FsStr,
    ) -> Result<NamespaceNode, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if !self.entry.node.is_dir() {
            return error!(ENOTDIR);
        }

        if basename.len() > NAME_MAX as usize {
            return error!(ENAMETOOLONG);
        }

        let child = if basename.is_empty() || basename == "." {
            self.clone()
        } else if basename == ".." {
            let root = match &context.resolve_base {
                ResolveBase::None => current_task.fs().root(),
                ResolveBase::Beneath(node) => {
                    // Do not allow traversal out of the 'node'.
                    if *self == *node {
                        return error!(EXDEV);
                    }
                    current_task.fs().root()
                }
                ResolveBase::InRoot(root) => root.clone(),
            };

            // Make sure this can't escape a chroot.
            if *self == root {
                root
            } else {
                self.parent().unwrap_or_else(|| self.clone())
            }
        } else {
            let mut child = self.with_new_entry(self.entry.component_lookup(
                locked,
                current_task,
                &self.mount,
                basename,
            )?);
            while child.entry.node.is_lnk() {
                match context.symlink_mode {
                    SymlinkMode::NoFollow => {
                        break;
                    }
                    SymlinkMode::Follow => {
                        if context.remaining_follows == 0
                            || context.resolve_flags.contains(ResolveFlags::NO_SYMLINKS)
                        {
                            return error!(ELOOP);
                        }
                        context.remaining_follows -= 1;
                        child = match child.readlink(locked, current_task)? {
                            SymlinkTarget::Path(link_target) => {
                                let link_directory = if link_target[0] == b'/' {
                                    match &context.resolve_base {
                                        ResolveBase::None => current_task.fs().root(),
                                        ResolveBase::Beneath(_) => return error!(EXDEV),
                                        ResolveBase::InRoot(root) => root.clone(),
                                    }
                                } else {
                                    self.clone()
                                };
                                current_task.lookup_path(
                                    locked,
                                    context,
                                    link_directory,
                                    link_target.as_ref(),
                                )?
                            }
                            SymlinkTarget::Node(node) => {
                                if context.resolve_flags.contains(ResolveFlags::NO_MAGICLINKS) {
                                    return error!(ELOOP);
                                }
                                node
                            }
                        }
                    }
                };
            }

            child.enter_mount()
        };

        if context.resolve_flags.contains(ResolveFlags::NO_XDEV) && child.mount != self.mount {
            return error!(EXDEV);
        }

        if context.must_be_directory && !child.entry.node.is_dir() {
            return error!(ENOTDIR);
        }

        Ok(child)
    }

    /// Traverse up a child-to-parent link in the namespace.
    ///
    /// This traversal matches the child-to-parent link in the underlying
    /// FsNode except at mountpoints, where the link switches from one
    /// filesystem to another.
    pub fn parent(&self) -> Option<NamespaceNode> {
        let mountpoint_or_self = self.escape_mount();
        Some(mountpoint_or_self.with_new_entry(mountpoint_or_self.entry.parent()?))
    }

    /// Returns the parent, but does not escape mounts i.e. returns None if this node
    /// is the root of a mount.
    pub fn parent_within_mount(&self) -> Option<DirEntryHandle> {
        if let Ok(_) = self.mount_if_root() {
            return None;
        }
        self.entry.parent()
    }

    /// Whether this namespace node is a descendant of the given node.
    ///
    /// Walks up the namespace node tree looking for ancestor. If ancestor is
    /// found, returns true. Otherwise, returns false.
    pub fn is_descendant_of(&self, ancestor: &NamespaceNode) -> bool {
        let ancestor = ancestor.escape_mount();
        let mut current = self.escape_mount();
        while current != ancestor {
            if let Some(parent) = current.parent() {
                current = parent.escape_mount();
            } else {
                return false;
            }
        }
        true
    }

    /// If this is a mount point, return the root of the mount. Otherwise return self.
    fn enter_mount(&self) -> NamespaceNode {
        // While the child is a mountpoint, replace child with the mount's root.
        fn enter_one_mount(node: &NamespaceNode) -> Option<NamespaceNode> {
            if let Some(mount) = node.mount.deref() {
                if let Some(submount) =
                    mount.state.read().submounts.get(ArcKey::ref_cast(&node.entry))
                {
                    return Some(submount.mount.root());
                }
            }
            None
        }
        let mut inner = self.clone();
        while let Some(inner_root) = enter_one_mount(&inner) {
            inner = inner_root;
        }
        inner
    }

    /// If this is the root of a mount, return the mount point. Otherwise return self.
    ///
    /// This is not exactly the same as parent(). If parent() is called on a root, it will escape
    /// the mount, but then return the parent of the mount point instead of the mount point.
    fn escape_mount(&self) -> NamespaceNode {
        let mut mountpoint_or_self = self.clone();
        while let Some(mountpoint) = mountpoint_or_self.mountpoint() {
            mountpoint_or_self = mountpoint;
        }
        mountpoint_or_self
    }

    /// If this node is the root of a mount, return it. Otherwise EINVAL.
    pub fn mount_if_root(&self) -> Result<&MountHandle, Errno> {
        if let Some(mount) = self.mount.deref() {
            if Arc::ptr_eq(&self.entry, &mount.root) {
                return Ok(mount);
            }
        }
        error!(EINVAL)
    }

    /// Returns the mountpoint at this location in the namespace.
    ///
    /// If this node is mounted in another node, this function returns the node
    /// at which this node is mounted. Otherwise, returns None.
    fn mountpoint(&self) -> Option<NamespaceNode> {
        self.mount_if_root().ok()?.mountpoint()
    }

    /// The path from the task's root to this node.
    pub fn path(&self, task: &Task) -> FsString {
        self.path_from_root(Some(&task.fs().root())).into_path()
    }

    /// The path from the root of the namespace to this node.
    pub fn path_escaping_chroot(&self) -> FsString {
        self.path_from_root(None).into_path()
    }

    /// Returns the path to this node, accounting for a custom root.
    /// A task may have a custom root set by `chroot`.
    pub fn path_from_root(&self, root: Option<&NamespaceNode>) -> PathWithReachability {
        if self.mount.is_none() {
            return PathWithReachability::Reachable(self.entry.local_name());
        }

        let mut path = PathBuilder::new();
        let mut current = self.escape_mount();
        if let Some(root) = root {
            // The current node is expected to intersect with the custom root as we travel up the tree.
            let root = root.escape_mount();
            while current != root {
                if let Some(parent) = current.parent() {
                    path.prepend_element(current.entry.local_name().as_ref());
                    current = parent.escape_mount();
                } else {
                    // This node hasn't intersected with the custom root and has reached the namespace root.
                    return PathWithReachability::Unreachable(path.build_absolute());
                }
            }
        } else {
            // No custom root, so travel up the tree to the namespace root.
            while let Some(parent) = current.parent() {
                path.prepend_element(current.entry.local_name().as_ref());
                current = parent.escape_mount();
            }
        }

        PathWithReachability::Reachable(path.build_absolute())
    }

    pub fn mount(&self, what: WhatToMount, flags: MountFlags) -> Result<(), Errno> {
        let flags = flags & (MountFlags::STORED_ON_MOUNT | MountFlags::REC);
        let mountpoint = self.enter_mount();
        let mount = mountpoint.mount.as_ref().expect("a mountpoint must be part of a mount");
        mount.create_submount(&mountpoint.entry, what, flags);
        Ok(())
    }

    /// If this is the root of a filesystem, unmount. Otherwise return EINVAL.
    pub fn unmount(&self, flags: UnmountFlags) -> Result<(), Errno> {
        let propagate = self.mount_if_root().map_or(false, |mount| mount.read().is_shared());
        let mount = self.enter_mount().mount_if_root()?.clone();
        mount.unmount(flags, propagate)
    }

    pub fn rename<L>(
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        old_parent: &NamespaceNode,
        old_name: &FsStr,
        new_parent: &NamespaceNode,
        new_name: &FsStr,
        flags: RenameFlags,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        DirEntry::rename(
            locked,
            current_task,
            &old_parent.entry,
            &old_parent.mount,
            old_name,
            &new_parent.entry,
            &new_parent.mount,
            new_name,
            flags,
        )
    }

    fn with_new_entry(&self, entry: DirEntryHandle) -> NamespaceNode {
        Self { mount: self.mount.clone(), entry }
    }

    fn mount_hash_key(&self) -> &ArcKey<DirEntry> {
        ArcKey::ref_cast(&self.entry)
    }

    pub fn suid_and_sgid(&self, current_task: &CurrentTask) -> Result<UserAndOrGroupId, Errno> {
        if self.mount.flags().contains(MountFlags::NOSUID) {
            Ok(UserAndOrGroupId::default())
        } else {
            let creds = current_task.creds();
            self.entry.node.info().suid_and_sgid(&creds)
        }
    }

    pub fn update_atime(&self) {
        // Do not update the atime of this node if it is mounted with the NOATIME flag.
        if !self.mount.flags().contains(MountFlags::NOATIME) {
            self.entry.node.update_info(|info| {
                let now = utc::utc_now();
                info.time_access = now;
            });
        }
    }

    pub fn readlink<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.update_atime();
        self.entry.node.readlink(locked, current_task)
    }

    pub fn notify(&self, event_mask: InotifyMask) {
        if self.mount.is_some() {
            self.entry.notify(event_mask);
        }
    }

    /// Check whether the node can be accessed in the current context with the specified access
    /// flags (read, write, or exec). Accounts for capabilities and whether the current user is the
    /// owner or is in the file's group.
    pub fn check_access<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        access: Access,
        reason: CheckAccessReason,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.entry.node.check_access(locked, current_task, &self.mount, access, reason)
    }

    /// Checks if O_NOATIME is allowed,
    pub fn check_o_noatime_allowed(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        self.entry.node.check_o_noatime_allowed(current_task)
    }

    pub fn truncate<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        length: u64,
    ) -> Result<(), Errno>
    where
        L: LockBefore<BeforeFsNodeAppend>,
    {
        self.entry.node.truncate(locked, current_task, &self.mount, length)?;
        self.entry.notify_ignoring_excl_unlink(InotifyMask::MODIFY);
        Ok(())
    }
}

impl fmt::Debug for NamespaceNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NamespaceNode")
            .field("path", &self.path_escaping_chroot())
            .field("mount", &self.mount)
            .field("entry", &self.entry)
            .finish()
    }
}

// Eq/Hash impls intended for the MOUNT_POINTS hash
impl PartialEq for NamespaceNode {
    fn eq(&self, other: &Self) -> bool {
        self.mount.as_ref().map(Arc::as_ptr).eq(&other.mount.as_ref().map(Arc::as_ptr))
            && Arc::ptr_eq(&self.entry, &other.entry)
    }
}
impl Eq for NamespaceNode {}
impl Hash for NamespaceNode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.mount.as_ref().map(Arc::as_ptr).hash(state);
        Arc::as_ptr(&self.entry).hash(state);
    }
}

/// A namespace node that keeps the underly mount busy.
#[derive(Debug, Clone)]
pub struct ActiveNamespaceNode {
    /// The underlying namespace node.
    name: NamespaceNode,

    /// Adds a reference to the mount client marker to prevent the mount from
    /// being removed while the NamespaceNode is active. Is None iff mount is
    /// None.
    _marker: Option<MountClientMarker>,
}

impl ActiveNamespaceNode {
    pub fn new(name: NamespaceNode) -> Self {
        let marker = name.mount.as_ref().map(|mount| mount.active_client_counter.clone());
        Self { name, _marker: marker }
    }

    pub fn to_passive(&self) -> NamespaceNode {
        self.deref().clone()
    }
}

impl Deref for ActiveNamespaceNode {
    type Target = NamespaceNode;

    fn deref(&self) -> &Self::Target {
        &self.name
    }
}

impl PartialEq for ActiveNamespaceNode {
    fn eq(&self, other: &Self) -> bool {
        self.deref().eq(other.deref())
    }
}
impl Eq for ActiveNamespaceNode {}
impl Hash for ActiveNamespaceNode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.deref().hash(state)
    }
}

/// Tracks all mounts, keyed by mount point.
pub struct Mounts {
    mounts: Mutex<HashMap<WeakKey<DirEntry>, Vec<ArcKey<Mount>>>>,
}

impl Mounts {
    pub fn new() -> Self {
        Mounts { mounts: Mutex::default() }
    }

    /// Registers the mount in the global mounts map.
    fn register_mount(&self, dir_entry: &Arc<DirEntry>, mount: MountHandle) -> Submount {
        let mut mounts = self.mounts.lock();
        mounts
            .entry(WeakKey::from(dir_entry))
            .or_insert_with(|| {
                dir_entry.set_has_mounts(true);
                Vec::new()
            })
            .push(ArcKey(mount.clone()));
        Submount { dir: ArcKey(dir_entry.clone()), mount }
    }

    /// Unregisters the mount.  This is called by `Submount::drop`.
    fn unregister_mount(&self, dir_entry: &Arc<DirEntry>, mount: &MountHandle) {
        let mut mounts = self.mounts.lock();
        let Entry::Occupied(mut o) = mounts.entry(WeakKey::from(dir_entry)) else {
            // This can happen if called from `unmount` below.
            return;
        };
        // This is O(N), but directory entries with large numbers of mounts should be rare.
        let index = o.get().iter().position(|e| e == ArcKey::ref_cast(mount)).unwrap();
        if o.get().len() == 1 {
            o.remove_entry();
            dir_entry.set_has_mounts(false);
        } else {
            o.get_mut().swap_remove(index);
        }
    }

    /// Unmounts all mounts associated with `dir_entry`.  This is called when `dir_entry` is
    /// unlinked (which would normally result in EBUSY, but not if it isn't mounted in the local
    /// namespace).
    pub fn unmount(&self, dir_entry: &DirEntry) {
        let mounts = self.mounts.lock().remove(&PtrKey::from(dir_entry as *const _));
        if let Some(mounts) = mounts {
            for mount in mounts {
                // Ignore errors.
                let _ = mount.unmount(UnmountFlags::default(), false);
            }
        }
    }
}

/// A RAII object that unregisters a mount when dropped.
#[derive(Debug)]
struct Submount {
    dir: ArcKey<DirEntry>,
    mount: MountHandle,
}

impl Drop for Submount {
    fn drop(&mut self) {
        self.mount.fs.kernel.upgrade().unwrap().mounts.unregister_mount(&self.dir, &self.mount)
    }
}

/// Submount is stored in a mount's submounts hash set, which is keyed by the mountpoint.
impl Eq for Submount {}
impl PartialEq<Self> for Submount {
    fn eq(&self, other: &Self) -> bool {
        self.dir == other.dir
    }
}
impl Hash for Submount {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.dir.hash(state)
    }
}

impl Borrow<ArcKey<DirEntry>> for Submount {
    fn borrow(&self) -> &ArcKey<DirEntry> {
        &self.dir
    }
}

#[cfg(test)]
mod test {
    use crate::fs::tmpfs::TmpFs;
    use crate::testing::create_kernel_task_and_unlocked;
    use crate::vfs::{
        LookupContext, Namespace, NamespaceNode, RenameFlags, UnlinkKind, WhatToMount,
    };
    use starnix_uapi::errno;
    use starnix_uapi::mount_flags::MountFlags;
    use std::sync::Arc;

    #[::fuchsia::test]
    async fn test_namespace() -> anyhow::Result<()> {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let root_fs = TmpFs::new_fs(&kernel);
        let root_node = Arc::clone(root_fs.root());
        let _dev_node = root_node
            .create_dir(&mut locked, &current_task, "dev".into())
            .expect("failed to mkdir dev");
        let dev_fs = TmpFs::new_fs(&kernel);
        let dev_root_node = Arc::clone(dev_fs.root());
        let _dev_pts_node = dev_root_node
            .create_dir(&mut locked, &current_task, "pts".into())
            .expect("failed to mkdir pts");

        let ns = Namespace::new(root_fs);
        let mut context = LookupContext::default();
        let dev = ns
            .root()
            .lookup_child(&mut locked, &current_task, &mut context, "dev".into())
            .expect("failed to lookup dev");
        dev.mount(WhatToMount::Fs(dev_fs), MountFlags::empty())
            .expect("failed to mount dev root node");

        let mut context = LookupContext::default();
        let dev = ns
            .root()
            .lookup_child(&mut locked, &current_task, &mut context, "dev".into())
            .expect("failed to lookup dev");
        let mut context = LookupContext::default();
        let pts = dev
            .lookup_child(&mut locked, &current_task, &mut context, "pts".into())
            .expect("failed to lookup pts");
        let pts_parent =
            pts.parent().ok_or_else(|| errno!(ENOENT)).expect("failed to get parent of pts");
        assert!(Arc::ptr_eq(&pts_parent.entry, &dev.entry));

        let dev_parent =
            dev.parent().ok_or_else(|| errno!(ENOENT)).expect("failed to get parent of dev");
        assert!(Arc::ptr_eq(&dev_parent.entry, &ns.root().entry));
        Ok(())
    }

    #[::fuchsia::test]
    async fn test_mount_does_not_upgrade() -> anyhow::Result<()> {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let root_fs = TmpFs::new_fs(&kernel);
        let root_node = Arc::clone(root_fs.root());
        let _dev_node = root_node
            .create_dir(&mut locked, &current_task, "dev".into())
            .expect("failed to mkdir dev");
        let dev_fs = TmpFs::new_fs(&kernel);
        let dev_root_node = Arc::clone(dev_fs.root());
        let _dev_pts_node = dev_root_node
            .create_dir(&mut locked, &current_task, "pts".into())
            .expect("failed to mkdir pts");

        let ns = Namespace::new(root_fs);
        let mut context = LookupContext::default();
        let dev = ns
            .root()
            .lookup_child(&mut locked, &current_task, &mut context, "dev".into())
            .expect("failed to lookup dev");
        dev.mount(WhatToMount::Fs(dev_fs), MountFlags::empty())
            .expect("failed to mount dev root node");
        let mut context = LookupContext::default();
        let new_dev = ns
            .root()
            .lookup_child(&mut locked, &current_task, &mut context, "dev".into())
            .expect("failed to lookup dev again");
        assert!(!Arc::ptr_eq(&dev.entry, &new_dev.entry));
        assert_ne!(&dev, &new_dev);

        let mut context = LookupContext::default();
        let _new_pts = new_dev
            .lookup_child(&mut locked, &current_task, &mut context, "pts".into())
            .expect("failed to lookup pts");
        let mut context = LookupContext::default();
        assert!(dev.lookup_child(&mut locked, &current_task, &mut context, "pts".into()).is_err());

        Ok(())
    }

    #[::fuchsia::test]
    async fn test_path() -> anyhow::Result<()> {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let root_fs = TmpFs::new_fs(&kernel);
        let root_node = Arc::clone(root_fs.root());
        let _dev_node = root_node
            .create_dir(&mut locked, &current_task, "dev".into())
            .expect("failed to mkdir dev");
        let dev_fs = TmpFs::new_fs(&kernel);
        let dev_root_node = Arc::clone(dev_fs.root());
        let _dev_pts_node = dev_root_node
            .create_dir(&mut locked, &current_task, "pts".into())
            .expect("failed to mkdir pts");

        let ns = Namespace::new(root_fs);
        let mut context = LookupContext::default();
        let dev = ns
            .root()
            .lookup_child(&mut locked, &current_task, &mut context, "dev".into())
            .expect("failed to lookup dev");
        dev.mount(WhatToMount::Fs(dev_fs), MountFlags::empty())
            .expect("failed to mount dev root node");

        let mut context = LookupContext::default();
        let dev = ns
            .root()
            .lookup_child(&mut locked, &current_task, &mut context, "dev".into())
            .expect("failed to lookup dev");
        let mut context = LookupContext::default();
        let pts = dev
            .lookup_child(&mut locked, &current_task, &mut context, "pts".into())
            .expect("failed to lookup pts");

        assert_eq!("/", ns.root().path_escaping_chroot());
        assert_eq!("/dev", dev.path_escaping_chroot());
        assert_eq!("/dev/pts", pts.path_escaping_chroot());
        Ok(())
    }

    #[::fuchsia::test]
    async fn test_shadowing() -> anyhow::Result<()> {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let root_fs = TmpFs::new_fs(&kernel);
        let ns = Namespace::new(root_fs.clone());
        let _foo_node = root_fs.root().create_dir(&mut locked, &current_task, "foo".into())?;
        let mut context = LookupContext::default();
        let foo_dir =
            ns.root().lookup_child(&mut locked, &current_task, &mut context, "foo".into())?;

        let foofs1 = TmpFs::new_fs(&kernel);
        foo_dir.mount(WhatToMount::Fs(foofs1.clone()), MountFlags::empty())?;
        let mut context = LookupContext::default();
        assert!(Arc::ptr_eq(
            &ns.root().lookup_child(&mut locked, &current_task, &mut context, "foo".into())?.entry,
            foofs1.root()
        ));
        let foo_dir =
            ns.root().lookup_child(&mut locked, &current_task, &mut context, "foo".into())?;

        let ns_clone = ns.clone_namespace();

        let foofs2 = TmpFs::new_fs(&kernel);
        foo_dir.mount(WhatToMount::Fs(foofs2.clone()), MountFlags::empty())?;
        let mut context = LookupContext::default();
        assert!(Arc::ptr_eq(
            &ns.root().lookup_child(&mut locked, &current_task, &mut context, "foo".into())?.entry,
            foofs2.root()
        ));

        assert!(Arc::ptr_eq(
            &ns_clone
                .root()
                .lookup_child(
                    &mut locked,
                    &current_task,
                    &mut LookupContext::default(),
                    "foo".into()
                )?
                .entry,
            foofs1.root()
        ));

        Ok(())
    }

    #[::fuchsia::test]
    async fn test_unlink_mounted_directory() -> anyhow::Result<()> {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let root_fs = TmpFs::new_fs(&kernel);
        let ns1 = Namespace::new(root_fs.clone());
        let ns2 = Namespace::new(root_fs.clone());
        let _foo_node = root_fs.root().create_dir(&mut locked, &current_task, "foo".into())?;
        let mut context = LookupContext::default();
        let foo_dir =
            ns1.root().lookup_child(&mut locked, &current_task, &mut context, "foo".into())?;

        let foofs = TmpFs::new_fs(&kernel);
        foo_dir.mount(WhatToMount::Fs(foofs), MountFlags::empty())?;

        // Trying to unlink from ns1 should fail.
        assert_eq!(
            ns1.root()
                .unlink(&mut locked, &current_task, "foo".into(), UnlinkKind::Directory, false)
                .unwrap_err(),
            errno!(EBUSY),
        );

        // But unlinking from ns2 should succeed.
        ns2.root()
            .unlink(&mut locked, &current_task, "foo".into(), UnlinkKind::Directory, false)
            .expect("unlink failed");

        // And it should no longer show up in ns1.
        assert_eq!(
            ns1.root()
                .unlink(&mut locked, &current_task, "foo".into(), UnlinkKind::Directory, false)
                .unwrap_err(),
            errno!(ENOENT),
        );

        Ok(())
    }

    #[::fuchsia::test]
    async fn test_rename_mounted_directory() -> anyhow::Result<()> {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let root_fs = TmpFs::new_fs(&kernel);
        let ns1 = Namespace::new(root_fs.clone());
        let ns2 = Namespace::new(root_fs.clone());
        let _foo_node = root_fs.root().create_dir(&mut locked, &current_task, "foo".into())?;
        let _bar_node = root_fs.root().create_dir(&mut locked, &current_task, "bar".into())?;
        let _baz_node = root_fs.root().create_dir(&mut locked, &current_task, "baz".into())?;
        let mut context = LookupContext::default();
        let foo_dir =
            ns1.root().lookup_child(&mut locked, &current_task, &mut context, "foo".into())?;

        let foofs = TmpFs::new_fs(&kernel);
        foo_dir.mount(WhatToMount::Fs(foofs), MountFlags::empty())?;

        // Trying to rename over foo from ns1 should fail.
        let root = ns1.root();
        assert_eq!(
            NamespaceNode::rename(
                &mut locked,
                &current_task,
                &root,
                "bar".into(),
                &root,
                "foo".into(),
                RenameFlags::empty()
            )
            .unwrap_err(),
            errno!(EBUSY),
        );
        // Likewise the other way.
        assert_eq!(
            NamespaceNode::rename(
                &mut locked,
                &current_task,
                &root,
                "foo".into(),
                &root,
                "bar".into(),
                RenameFlags::empty()
            )
            .unwrap_err(),
            errno!(EBUSY),
        );

        // But renaming from ns2 should succeed.
        let root = ns2.root();

        // First rename the directory with the mount.
        NamespaceNode::rename(
            &mut locked,
            &current_task,
            &root,
            "foo".into(),
            &root,
            "bar".into(),
            RenameFlags::empty(),
        )
        .expect("rename failed");

        // Renaming over a directory with a mount should also work.
        NamespaceNode::rename(
            &mut locked,
            &current_task,
            &root,
            "baz".into(),
            &root,
            "bar".into(),
            RenameFlags::empty(),
        )
        .expect("rename failed");

        // "foo" and "baz" should no longer show up in ns1.
        assert_eq!(
            ns1.root()
                .lookup_child(&mut locked, &current_task, &mut context, "foo".into())
                .unwrap_err(),
            errno!(ENOENT)
        );
        assert_eq!(
            ns1.root()
                .lookup_child(&mut locked, &current_task, &mut context, "baz".into())
                .unwrap_err(),
            errno!(ENOENT)
        );

        Ok(())
    }
}
