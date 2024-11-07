// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::security;
use crate::task::CurrentTask;
use crate::vfs::{
    path, CheckAccessReason, FileHandle, FileObject, FsNodeHandle, FsNodeLinkBehavior, FsStr,
    FsString, MountInfo, Mounts, NamespaceNode, UnlinkKind,
};
use bitflags::bitflags;
use starnix_sync::{FileOpsCore, LockBefore, LockEqualOrBefore, Locked, RwLock, RwLockWriteGuard};
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::{Errno, ENOENT};
use starnix_uapi::file_mode::{Access, FileMode};
use starnix_uapi::inotify_mask::InotifyMask;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error, NAME_MAX, RENAME_EXCHANGE, RENAME_NOREPLACE, RENAME_WHITEOUT};
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Weak};

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct RenameFlags: u32 {
        // Exchange the entries.
        const EXCHANGE = RENAME_EXCHANGE;

        // Don't overwrite an existing DirEntry.
        const NOREPLACE = RENAME_NOREPLACE;

        // Create a "whiteout" object to replace the file.
        const WHITEOUT = RENAME_WHITEOUT;

        // Allow replacing any file with a directory. This is an internal flag used only
        // internally inside Starnix for OverlayFS.
        const REPLACE_ANY = 0x80000000;

        // Internal flags that cannot be passed to `sys_rename()`
        const INTERNAL = Self::REPLACE_ANY.bits();
    }
}

struct DirEntryState {
    /// The parent DirEntry.
    ///
    /// The DirEntry tree has strong references from child-to-parent and weak
    /// references from parent-to-child. This design ensures that the parent
    /// chain is always populated in the cache, but some children might be
    /// missing from the cache.
    parent: Option<DirEntryHandle>,

    /// The name that this parent calls this child.
    ///
    /// This name might not be reflected in the full path in the namespace that
    /// contains this DirEntry. For example, this DirEntry might be the root of
    /// a chroot.
    ///
    /// Most callers that want to work with names for DirEntries should use the
    /// NamespaceNodes.
    local_name: FsString,

    /// Whether this directory entry has been removed from the tree.
    is_dead: bool,

    /// Whether the entry has filesystems mounted on top of it.
    has_mounts: bool,
}

pub trait DirEntryOps: Send + Sync + 'static {
    /// Revalidate the [`DirEntry`], if needed.
    ///
    /// Most filesystems don't need to do any revalidations because they are "local"
    /// and all changes to nodes go through the kernel. However some filesystems
    /// allow changes to happen through other means (e.g. NFS, FUSE) and these
    /// filesystems need a way to let the kernel know it may need to refresh its
    /// cached metadata. This method provides that hook for such filesystems.
    ///
    /// For more details, see:
    ///  - https://www.halolinux.us/kernel-reference/the-dentry-cache.html
    ///  - https://www.kernel.org/doc/html/latest/filesystems/path-lookup.html#revalidation-and-automounts
    ///  - https://lwn.net/Articles/649115/
    ///  - https://www.infradead.org/~mchehab/kernel_docs/filesystems/path-walking.html
    ///
    /// Returns `Ok(valid)` where `valid` indicates if the `DirEntry` is still valid,
    /// or an error.
    fn revalidate(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _: &CurrentTask,
        _: &DirEntry,
    ) -> Result<bool, Errno> {
        Ok(true)
    }
}

pub struct DefaultDirEntryOps;

impl DirEntryOps for DefaultDirEntryOps {}

/// An entry in a directory.
///
/// This structure assigns a name to an FsNode in a given file system. An
/// FsNode might have multiple directory entries, for example if there are more
/// than one hard link to the same FsNode. In those cases, each hard link will
/// have a different parent and a different local_name because each hard link
/// has its own DirEntry object.
///
/// A directory cannot have more than one hard link, which means there is a
/// single DirEntry for each Directory FsNode. That invariant lets us store the
/// children for a directory in the DirEntry rather than in the FsNode.
pub struct DirEntry {
    /// The FsNode referenced by this DirEntry.
    ///
    /// A given FsNode can be referenced by multiple DirEntry objects, for
    /// example if there are multiple hard links to a given FsNode.
    pub node: FsNodeHandle,

    /// The [`DirEntryOps`] for this `DirEntry`.
    ///
    /// The `DirEntryOps` are implemented by the individual file systems to provide
    /// specific behaviours for this `DirEntry`.
    ops: Box<dyn DirEntryOps>,

    /// The mutable state for this DirEntry.
    ///
    /// Leaf lock - do not acquire other locks while holding this one.
    state: RwLock<DirEntryState>,

    /// A partial cache of the children of this DirEntry.
    ///
    /// DirEntries are added to this cache when they are looked up and removed
    /// when they are no longer referenced.
    ///
    /// This is separated from the DirEntryState for lock ordering. rename needs to lock the source
    /// parent, the target parent, the source, and the target - four (4) DirEntries in total.
    /// Getting the ordering right on these is nearly impossible. However, we only need to lock the
    /// children map on the two parents and we don't need to lock the children map on the two
    /// children. So splitting the children out into its own lock resolves this.
    children: RwLock<DirEntryChildren>,
}
type DirEntryChildren = BTreeMap<FsString, Weak<DirEntry>>;

pub type DirEntryHandle = Arc<DirEntry>;

impl DirEntry {
    #[allow(clippy::let_and_return)]
    pub fn new(
        node: FsNodeHandle,
        parent: Option<DirEntryHandle>,
        local_name: FsString,
    ) -> DirEntryHandle {
        let ops = node.create_dir_entry_ops();
        let result = Arc::new(DirEntry {
            node,
            ops,
            state: RwLock::new(DirEntryState {
                parent,
                local_name,
                is_dead: false,
                has_mounts: false,
            }),
            children: Default::default(),
        });
        #[cfg(any(test, debug_assertions))]
        {
            let _l1 = result.children.read();
            let _l2 = result.state.read();
        }
        result
    }

    /// Returns a new DirEntry for the given `node` without parent. The entry has no local name.
    pub fn new_unrooted(node: FsNodeHandle) -> DirEntryHandle {
        Self::new(node, None, FsString::default())
    }

    /// Returns a file handle to this entry, associated with an anonymous namespace.
    pub fn open_anonymous<L>(
        self: &DirEntryHandle,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<FileHandle, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        FileObject::new(
            current_task,
            self.node.create_file_ops(locked, current_task, flags)?,
            NamespaceNode::new_anonymous(self.clone()),
            flags,
        )
    }

    fn lock_children<'a>(self: &'a DirEntryHandle) -> DirEntryLockedChildren<'a> {
        DirEntryLockedChildren { entry: self, children: self.children.write() }
    }

    /// The name that this node's parent calls this node.
    ///
    /// If this node is mounted in a namespace, the parent of this node in that
    /// namespace might have a different name for the point in the namespace at
    /// which this node is mounted.
    pub fn local_name(&self) -> FsString {
        self.state.read().local_name.clone()
    }

    /// The parent DirEntry object.
    ///
    /// Returns None if this DirEntry is the root of its file system.
    ///
    /// Be aware that the root of one file system might be mounted as a child
    /// in another file system. For that reason, consider walking the
    /// NamespaceNode tree (which understands mounts) rather than the DirEntry
    /// tree.
    pub fn parent(&self) -> Option<DirEntryHandle> {
        self.state.read().parent.clone()
    }

    /// The parent DirEntry object or this DirEntry if this entry is the root.
    ///
    /// Useful when traversing up the tree if you always want to find a parent
    /// (e.g., for "..").
    ///
    /// Be aware that the root of one file system might be mounted as a child
    /// in another file system. For that reason, consider walking the
    /// NamespaceNode tree (which understands mounts) rather than the DirEntry
    /// tree.
    pub fn parent_or_self(self: &DirEntryHandle) -> DirEntryHandle {
        self.state.read().parent.as_ref().unwrap_or(self).clone()
    }

    /// Whether this directory entry has been removed from the tree.
    pub fn is_dead(&self) -> bool {
        self.state.read().is_dead
    }

    /// Whether the given name has special semantics as a directory entry.
    ///
    /// Specifically, whether the name is empty (which means "self"), dot
    /// (which also means "self"), or dot dot (which means "parent").
    pub fn is_reserved_name(name: &FsStr) -> bool {
        name.is_empty() || name == "." || name == ".."
    }

    /// Look up a directory entry with the given name as direct child of this
    /// entry.
    pub fn component_lookup<L>(
        self: &DirEntryHandle,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
    ) -> Result<DirEntryHandle, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let (node, _) = self.get_or_create_child(
            locked,
            current_task,
            mount,
            name,
            |locked, d, mount, name| d.lookup(locked, current_task, mount, name),
        )?;
        Ok(node)
    }

    /// Creates a new DirEntry
    ///
    /// The create_node_fn function is called to create the underlying FsNode
    /// for the DirEntry.
    ///
    /// If the entry already exists, create_node_fn is not called, and EEXIST is
    /// returned.
    pub fn create_entry<L>(
        self: &DirEntryHandle,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        create_node_fn: impl FnOnce(
            &mut Locked<'_, L>,
            &FsNodeHandle,
            &MountInfo,
            &FsStr,
        ) -> Result<FsNodeHandle, Errno>,
    ) -> Result<DirEntryHandle, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let (entry, exists) =
            self.create_entry_internal(locked, current_task, mount, name, create_node_fn)?;
        if exists {
            return error!(EEXIST);
        }
        Ok(entry)
    }

    /// Creates a new DirEntry. Works just like create_entry, except if the entry already exists,
    /// it is returned.
    pub fn get_or_create_entry<L>(
        self: &DirEntryHandle,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        create_node_fn: impl FnOnce(
            &mut Locked<'_, L>,
            &FsNodeHandle,
            &MountInfo,
            &FsStr,
        ) -> Result<FsNodeHandle, Errno>,
    ) -> Result<DirEntryHandle, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let (entry, _exists) =
            self.create_entry_internal(locked, current_task, mount, name, create_node_fn)?;
        Ok(entry)
    }

    fn create_entry_internal<L>(
        self: &DirEntryHandle,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        create_node_fn: impl FnOnce(
            &mut Locked<'_, L>,
            &FsNodeHandle,
            &MountInfo,
            &FsStr,
        ) -> Result<FsNodeHandle, Errno>,
    ) -> Result<(DirEntryHandle, bool), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if DirEntry::is_reserved_name(name) {
            return error!(EEXIST);
        }
        // TODO: Do we need to check name for embedded NUL characters?
        if name.len() > NAME_MAX as usize {
            return error!(ENAMETOOLONG);
        }
        if name.contains(&path::SEPARATOR) {
            return error!(EINVAL);
        }
        let (entry, exists) =
            self.get_or_create_child(locked, current_task, mount, name, create_node_fn)?;
        if !exists {
            // An entry was created. Update the ctime and mtime of this directory.
            self.node.update_ctime_mtime();
            entry.notify_creation();
        }
        Ok((entry, exists))
    }

    // This is marked as test-only because it sets the owner/group to root instead of the current
    // user to save a bit of typing in tests, but this shouldn't happen silently in production.
    #[cfg(test)]
    pub fn create_dir<L>(
        self: &DirEntryHandle,
        locked: &mut starnix_sync::Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<DirEntryHandle, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        self.create_dir_for_testing(locked, current_task, name)
    }

    // This function is for testing because it sets the owner/group to root instead of the current
    // user to save a bit of typing in tests, but this shouldn't happen silently in production.
    pub fn create_dir_for_testing<L>(
        self: &DirEntryHandle,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<DirEntryHandle, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        // TODO: apply_umask
        self.create_entry(
            locked,
            current_task,
            &MountInfo::detached(),
            name,
            |locked, dir, mount, name| {
                dir.mknod(
                    locked,
                    current_task,
                    mount,
                    name,
                    starnix_uapi::file_mode::mode!(IFDIR, 0o777),
                    starnix_uapi::device_type::DeviceType::NONE,
                    FsCred::root(),
                )
            },
        )
    }

    /// Creates an anonymous file.
    ///
    /// The FileMode::IFMT of the FileMode is always FileMode::IFREG.
    ///
    /// Used by O_TMPFILE.
    pub fn create_tmpfile<L>(
        self: &DirEntryHandle,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        mode: FileMode,
        owner: FsCred,
        flags: OpenFlags,
    ) -> Result<DirEntryHandle, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        // Only directories can have children.
        if !self.node.is_dir() {
            return error!(ENOTDIR);
        }
        assert!(mode.is_reg());

        // From <https://man7.org/linux/man-pages/man2/open.2.html>:
        //
        //   Specifying O_EXCL in conjunction with O_TMPFILE prevents a
        //   temporary file from being linked into the filesystem in
        //   the above manner.  (Note that the meaning of O_EXCL in
        //   this case is different from the meaning of O_EXCL
        //   otherwise.)
        let link_behavior = if flags.contains(OpenFlags::EXCL) {
            FsNodeLinkBehavior::Disallowed
        } else {
            FsNodeLinkBehavior::Allowed
        };

        let node =
            self.node.create_tmpfile(locked, current_task, mount, mode, owner, link_behavior)?;
        Ok(DirEntry::new_unrooted(node))
    }

    pub fn unlink<L>(
        self: &DirEntryHandle,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        kind: UnlinkKind,
        must_be_directory: bool,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        assert!(!DirEntry::is_reserved_name(name));

        // child *must* be dropped after self_children and child_children below (even in the error
        // paths).
        let child;

        let mut self_children = self.lock_children();
        child = self_children.component_lookup(locked, current_task, mount, name)?;
        let child_children = child.children.read();

        child.require_no_mounts(mount)?;

        // Check that this filesystem entry must be a directory. This can
        // happen if the path terminates with a trailing slash.
        //
        // Example: If we're unlinking a symlink `/foo/bar/`, this would
        // result in `ENOTDIR` because of the trailing slash, even if
        // `UnlinkKind::NonDirectory` was used.
        if must_be_directory && !child.node.is_dir() {
            return error!(ENOTDIR);
        }

        match kind {
            UnlinkKind::Directory => {
                if !child.node.is_dir() {
                    return error!(ENOTDIR);
                }
                // This check only covers whether the cache is non-empty.
                // We actually need to check whether the underlying directory is
                // empty by asking the node via remove below.
                if !child_children.is_empty() {
                    return error!(ENOTEMPTY);
                }
            }
            UnlinkKind::NonDirectory => {
                if child.node.is_dir() {
                    return error!(EISDIR);
                }
            }
        }

        self.node.unlink(locked, current_task, mount, name, &child.node)?;
        self_children.children.remove(name);

        std::mem::drop(child_children);
        std::mem::drop(self_children);

        child.destroy(&current_task.kernel().mounts);

        Ok(())
    }

    /// Destroy this directory entry.
    ///
    /// Notice that this method takes `self` by value to destroy this reference.
    fn destroy(self: DirEntryHandle, mounts: &Mounts) {
        let unmount = {
            let mut state = self.state.write();
            if state.is_dead {
                return;
            }
            state.is_dead = true;
            std::mem::replace(&mut state.has_mounts, false)
        };
        self.node.fs().will_destroy_dir_entry(&self);
        if unmount {
            mounts.unmount(&self);
        }
        self.notify_deletion();
    }

    /// Returns whether this entry is a descendant of |other|.
    pub fn is_descendant_of(self: &DirEntryHandle, other: &DirEntryHandle) -> bool {
        let mut current = self.clone();
        loop {
            if Arc::ptr_eq(&current, other) {
                // We found |other|.
                return true;
            }
            if let Some(next) = current.parent() {
                current = next;
            } else {
                // We reached the root of the file system.
                return false;
            }
        }
    }

    /// Rename the file with old_basename in old_parent to new_basename in
    /// new_parent.
    ///
    /// old_parent and new_parent must belong to the same file system.
    pub fn rename<L>(
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        old_parent: &DirEntryHandle,
        old_mount: &MountInfo,
        old_basename: &FsStr,
        new_parent: &DirEntryHandle,
        new_mount: &MountInfo,
        new_basename: &FsStr,
        flags: RenameFlags,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        // The nodes we are touching must be part of the same mount.
        if old_mount != new_mount {
            return error!(EXDEV);
        }
        // The mounts are equals, choose one.
        let mount = old_mount;

        // If either the old_basename or the new_basename is a reserved name
        // (e.g., "." or ".."), then we cannot do the rename.
        if DirEntry::is_reserved_name(old_basename) || DirEntry::is_reserved_name(new_basename) {
            if flags.contains(RenameFlags::NOREPLACE) {
                return error!(EEXIST);
            }
            return error!(EBUSY);
        }

        // If the names and parents are the same, then there's nothing to do
        // and we can report success.
        if Arc::ptr_eq(&old_parent.node, &new_parent.node) && old_basename == new_basename {
            return Ok(());
        }

        // This task must have write access to the old and new parent nodes.
        old_parent.node.check_access(
            locked,
            current_task,
            mount,
            Access::WRITE,
            CheckAccessReason::InternalPermissionChecks,
        )?;
        new_parent.node.check_access(
            locked,
            current_task,
            mount,
            Access::WRITE,
            CheckAccessReason::InternalPermissionChecks,
        )?;

        // The mount check ensures that the nodes we're touching are part of the
        // same file system. It doesn't matter where we grab the FileSystem reference from.
        let fs = old_parent.node.fs();

        // We need to hold these DirEntryHandles until after we drop all the
        // locks so that we do not deadlock when we drop them.
        let renamed;
        let mut maybe_replaced = None;

        {
            // Before we take any locks, we need to take the rename mutex on
            // the file system. This lock ensures that no other rename
            // operations are happening in this file system while we're
            // analyzing this rename operation.
            //
            // For example, we grab writer locks on both old_parent and
            // new_parent. If there was another rename operation in flight with
            // old_parent and new_parent reversed, then we could deadlock while
            // trying to acquire these locks.
            let _lock = fs.rename_mutex.lock();

            // Compute the list of ancestors of new_parent to check whether new_parent is a
            // descendant of the renamed node. This must be computed before taking any lock to
            // prevent lock inversions.
            let mut new_parent_ancestor_list = Vec::<DirEntryHandle>::new();
            {
                let mut current = Some(new_parent.clone());
                while let Some(entry) = current {
                    current = entry.parent();
                    new_parent_ancestor_list.push(entry);
                }
            }

            // We cannot simply grab the locks on old_parent and new_parent
            // independently because old_parent and new_parent might be the
            // same directory entry. Instead, we use the RenameGuard helper to
            // grab the appropriate locks.
            let mut state = RenameGuard::lock(old_parent, new_parent);

            // Now that we know the old_parent child list cannot change, we
            // establish the DirEntry that we are going to try to rename.
            renamed =
                state.old_parent().component_lookup(locked, current_task, mount, old_basename)?;

            // Check whether the sticky bit on the old parent prevents us from
            // removing this child.
            old_parent.node.check_sticky_bit(current_task, &renamed.node)?;

            // If new_parent is a descendant of renamed, the operation would
            // create a cycle. That's disallowed.
            if new_parent_ancestor_list.into_iter().any(|entry| Arc::ptr_eq(&entry, &renamed)) {
                return error!(EINVAL);
            }

            // Check whether the renamed entry is a mountpoint.
            // TODO: We should hold a read lock on the mount points for this
            //       namespace to prevent the child from becoming a mount point
            //       while this function is executing.
            renamed.require_no_mounts(mount)?;

            // We need to check if there is already a DirEntry with
            // new_basename in new_parent. If so, there are additional checks
            // we need to perform.
            match state.new_parent().component_lookup(locked, current_task, mount, new_basename) {
                Ok(replaced) => {
                    // Set `maybe_replaced` now to ensure it gets dropped in the right order.
                    let replaced = maybe_replaced.insert(replaced);

                    if flags.contains(RenameFlags::NOREPLACE) {
                        return error!(EEXIST);
                    }

                    // Sayeth https://man7.org/linux/man-pages/man2/rename.2.html:
                    //
                    // "If oldpath and newpath are existing hard links referring to the
                    // same file, then rename() does nothing, and returns a success
                    // status."
                    if Arc::ptr_eq(&renamed.node, &replaced.node) {
                        return Ok(());
                    }

                    // Sayeth https://man7.org/linux/man-pages/man2/rename.2.html:
                    //
                    // "oldpath can specify a directory.  In this case, newpath must"
                    // either not exist, or it must specify an empty directory."
                    if replaced.node.is_dir() {
                        // Check whether the replaced entry is a mountpoint.
                        // TODO: We should hold a read lock on the mount points for this
                        //       namespace to prevent the child from becoming a mount point
                        //       while this function is executing.
                        replaced.require_no_mounts(mount)?;
                    }

                    if !flags.intersects(RenameFlags::EXCHANGE | RenameFlags::REPLACE_ANY) {
                        if renamed.node.is_dir() && !replaced.node.is_dir() {
                            return error!(ENOTDIR);
                        } else if !renamed.node.is_dir() && replaced.node.is_dir() {
                            return error!(EISDIR);
                        }
                    }
                }
                // It's fine for the lookup to fail to find a child.
                Err(errno) if errno == ENOENT => {}
                // However, other errors are fatal.
                Err(e) => return Err(e),
            }

            // We've found all the errors that we know how to find. Ask the
            // file system to actually execute the rename operation. Once the
            // file system has executed the rename, we are no longer allowed to
            // fail because we will not be able to return the system to a
            // consistent state.

            if flags.contains(RenameFlags::EXCHANGE) {
                let replaced = maybe_replaced.as_ref().ok_or_else(|| errno!(ENOENT))?;
                fs.exchange(
                    current_task,
                    &renamed.node,
                    &old_parent.node,
                    old_basename,
                    &replaced.node,
                    &new_parent.node,
                    new_basename,
                )?;
            } else {
                fs.rename(
                    locked,
                    current_task,
                    &old_parent.node,
                    old_basename,
                    &new_parent.node,
                    new_basename,
                    &renamed.node,
                    maybe_replaced.as_ref().map(|replaced| &replaced.node),
                )?;
            }

            {
                // We need to update the parent and local name for the DirEntry
                // we are renaming to reflect its new parent and its new name.
                let mut renamed_state = renamed.state.write();
                renamed_state.parent = Some(new_parent.clone());
                renamed_state.local_name = new_basename.into();
            }
            // Actually add the renamed child to the new_parent's child list.
            // This operation implicitly removes the replaced child (if any)
            // from the child list.
            state.new_parent().children.insert(new_basename.into(), Arc::downgrade(&renamed));

            if flags.contains(RenameFlags::EXCHANGE) {
                // Reparent `replaced` when exchanging.
                let replaced =
                    maybe_replaced.as_ref().expect("replaced expected with RENAME_EXCHANGE");
                {
                    let mut replaced_state = replaced.state.write();
                    replaced_state.parent = Some(old_parent.clone());
                    replaced_state.local_name = old_basename.into();
                }
                state.old_parent().children.insert(old_basename.into(), Arc::downgrade(replaced));
            } else {
                // Remove the renamed child from the old_parent's child list.
                state.old_parent().children.remove(old_basename);
            }
        };

        fs.purge_old_entries();

        if let Some(replaced) = maybe_replaced {
            if !flags.contains(RenameFlags::EXCHANGE) {
                replaced.destroy(&current_task.kernel().mounts);
            }
        }

        // Renaming a file updates its ctime.
        renamed.node.update_ctime();

        let mode = renamed.node.info().mode;
        let cookie = current_task.kernel().get_next_inotify_cookie();
        old_parent.node.watchers.notify(InotifyMask::MOVE_FROM, cookie, old_basename, mode, false);
        new_parent.node.watchers.notify(InotifyMask::MOVE_TO, cookie, new_basename, mode, false);
        renamed.node.watchers.notify(InotifyMask::MOVE_SELF, 0, Default::default(), mode, false);

        Ok(())
    }

    pub fn get_children<F, T>(&self, callback: F) -> T
    where
        F: FnOnce(&DirEntryChildren) -> T,
    {
        let children = self.children.read();
        callback(&children)
    }

    /// Remove the child with the given name from the children cache.  The child must not have any
    /// mounts.
    pub fn remove_child(&self, name: &FsStr, mounts: &Mounts) {
        let mut children = self.children.write();
        let child = children.get(name).and_then(Weak::upgrade);
        if let Some(child) = child {
            children.remove(name);
            std::mem::drop(children);
            child.destroy(mounts);
        }
    }

    fn get_or_create_child<L>(
        self: &DirEntryHandle,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        create_fn: impl FnOnce(
            &mut Locked<'_, L>,
            &FsNodeHandle,
            &MountInfo,
            &FsStr,
        ) -> Result<FsNodeHandle, Errno>,
    ) -> Result<(DirEntryHandle, bool), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        assert!(!DirEntry::is_reserved_name(name));
        // Only directories can have children.
        if !self.node.is_dir() {
            return error!(ENOTDIR);
        }
        // The user must be able to search the directory (requires the EXEC permission)
        self.node.check_access(
            locked,
            current_task,
            mount,
            Access::EXEC,
            CheckAccessReason::InternalPermissionChecks,
        )?;

        // Check if the child is already in children. In that case, we can
        // simply return the child and we do not need to call init_fn.
        let child = self.children.read().get(name).and_then(Weak::upgrade);
        let (child, create_result) = if let Some(child) = child {
            child.node.fs().did_access_dir_entry(&child);
            (child, CreationResult::Existed { create_fn })
        } else {
            let (child, create_result) = self.lock_children().get_or_create_child(
                locked,
                current_task,
                mount,
                name,
                create_fn,
            )?;
            child.node.fs().purge_old_entries();
            (child, create_result)
        };

        let (child, exists) = match create_result {
            CreationResult::Created => (child, false),
            CreationResult::Existed { create_fn } => {
                if child.ops.revalidate(
                    &mut locked.cast_locked::<FileOpsCore>(),
                    current_task,
                    &child,
                )? {
                    (child, true)
                } else {
                    self.internal_remove_child(&child);
                    child.destroy(&current_task.kernel().mounts);

                    let (child, create_result) = self.lock_children().get_or_create_child(
                        locked,
                        current_task,
                        mount,
                        name,
                        create_fn,
                    )?;
                    child.node.fs().purge_old_entries();
                    (child, matches!(create_result, CreationResult::Existed { .. }))
                }
            }
        };

        Ok((child, exists))
    }

    // This function is only useful for tests and has some oddities.
    //
    // For example, not all the children might have been looked up yet, which
    // means the returned vector could be missing some names.
    //
    // Also, the vector might have "extra" names that are in the process of
    // being looked up. If the lookup fails, they'll be removed.
    #[cfg(test)]
    pub fn copy_child_names(&self) -> Vec<FsString> {
        self.children
            .read()
            .values()
            .filter_map(|child| Weak::upgrade(child).map(|c| c.local_name()))
            .collect()
    }

    fn internal_remove_child(&self, child: &DirEntry) {
        let local_name = child.local_name();
        let mut children = self.children.write();
        if let Some(weak_child) = children.get(&local_name) {
            // If this entry is occupied, we need to check whether child is
            // the current occupant. If so, we should remove the entry
            // because the child no longer exists.
            if std::ptr::eq(weak_child.as_ptr(), child) {
                children.remove(&local_name);
            }
        }
    }

    /// Notifies watchers on the current node and its parent about an event.
    pub fn notify(&self, event_mask: InotifyMask) {
        self.notify_watchers(event_mask, self.is_dead());
    }

    /// Notifies watchers on the current node and its parent about an event.
    ///
    /// Used for FSNOTIFY_EVENT_INODE events, which ignore IN_EXCL_UNLINK.
    pub fn notify_ignoring_excl_unlink(&self, event_mask: InotifyMask) {
        // We pretend that this directory entry is not dead to ignore IN_EXCL_UNLINK.
        self.notify_watchers(event_mask, false);
    }

    fn notify_watchers(&self, event_mask: InotifyMask, is_dead: bool) {
        let mode = self.node.info().mode;
        if let Some(parent) = self.parent() {
            parent.node.watchers.notify(event_mask, 0, self.local_name().as_ref(), mode, is_dead);
        }
        self.node.watchers.notify(event_mask, 0, Default::default(), mode, is_dead);
    }

    /// Notifies parents about creation, and notifies current node about link_count change.
    fn notify_creation(&self) {
        let mode = self.node.info().mode;
        if Arc::strong_count(&self.node) > 1 {
            // Notify about link change only if there is already a hardlink.
            self.node.watchers.notify(InotifyMask::ATTRIB, 0, Default::default(), mode, false);
        }
        if let Some(parent) = self.parent() {
            parent.node.watchers.notify(
                InotifyMask::CREATE,
                0,
                self.local_name().as_ref(),
                mode,
                false,
            );
        }
    }

    /// Notifies watchers on the current node about deletion if this is the
    /// last hardlink, and drops the DirEntryHandle kept by Inotify.
    /// Parent is also notified about deletion.
    fn notify_deletion(&self) {
        let mode = self.node.info().mode;
        if !mode.is_dir() {
            // Linux notifies link count change for non-directories.
            self.node.watchers.notify(InotifyMask::ATTRIB, 0, Default::default(), mode, false);
        }

        if let Some(parent) = self.parent() {
            parent.node.watchers.notify(
                InotifyMask::DELETE,
                0,
                self.local_name().as_ref(),
                mode,
                false,
            );
        }

        // This check is incorrect if there's another hard link to this FsNode that isn't in
        // memory at the moment.
        if Arc::strong_count(&self.node) == 1 {
            self.node.watchers.notify(InotifyMask::DELETE_SELF, 0, Default::default(), mode, false);
        }
    }

    /// Returns true if this entry has mounts.
    pub fn has_mounts(&self) -> bool {
        self.state.read().has_mounts
    }

    /// Records whether or not the entry has mounts.
    pub fn set_has_mounts(&self, v: bool) {
        self.state.write().has_mounts = v;
    }

    /// Verifies this directory has nothing mounted on it.
    fn require_no_mounts(self: &Arc<Self>, parent_mount: &MountInfo) -> Result<(), Errno> {
        if self.state.read().has_mounts {
            if let Some(mount) = parent_mount.as_ref() {
                if mount.has_submount(self) {
                    return error!(EBUSY);
                }
            }
        }
        Ok(())
    }
}

struct DirEntryLockedChildren<'a> {
    entry: &'a DirEntryHandle,
    children: RwLockWriteGuard<'a, DirEntryChildren>,
}

enum CreationResult<F> {
    Created,
    Existed { create_fn: F },
}

impl<'a> DirEntryLockedChildren<'a> {
    fn component_lookup<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
    ) -> Result<DirEntryHandle, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        assert!(!DirEntry::is_reserved_name(name));
        let (node, _) =
            self.get_or_create_child(locked, current_task, mount, name, |_, _, _, _| {
                error!(ENOENT)
            })?;
        Ok(node)
    }

    fn get_or_create_child<
        L,
        F: FnOnce(
            &mut Locked<'_, L>,
            &FsNodeHandle,
            &MountInfo,
            &FsStr,
        ) -> Result<FsNodeHandle, Errno>,
    >(
        &mut self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        create_fn: F,
    ) -> Result<(DirEntryHandle, CreationResult<F>), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let create_child = |locked: &mut Locked<'_, L>, create_fn: F| {
            // Before creating the child, check for existence.
            let (node, create_result) =
                match self.entry.node.lookup(locked, current_task, mount, name) {
                    Ok(node) => (node, CreationResult::Existed { create_fn }),
                    Err(e) if e == ENOENT => {
                        (create_fn(locked, &self.entry.node, mount, name)?, CreationResult::Created)
                    }
                    Err(e) => return Err(e),
                };

            assert!(
                node.info().mode & FileMode::IFMT != FileMode::EMPTY,
                "FsNode initialization did not populate the FileMode in FsNodeInfo."
            );

            let entry = DirEntry::new(node, Some(self.entry.clone()), name.to_owned());
            #[cfg(any(test, debug_assertions))]
            {
                // Take the lock on child while holding the one on the parent to ensure any wrong
                // ordering will trigger the tracing-mutex at the right call site.
                let _l1 = entry.state.read();
            }
            Ok((entry, create_result))
        };

        let (child, create_result) = match self.children.entry(name.to_owned()) {
            Entry::Vacant(entry) => {
                let (child, create_result) = create_child(locked, create_fn)?;
                entry.insert(Arc::downgrade(&child));
                (child, create_result)
            }
            Entry::Occupied(mut entry) => {
                // It's possible that the upgrade will succeed this time around because we dropped
                // the read lock before acquiring the write lock. Another thread might have
                // populated this entry while we were not holding any locks.
                if let Some(child) = Weak::upgrade(entry.get()) {
                    child.node.fs().did_access_dir_entry(&child);
                    return Ok((child, CreationResult::Existed { create_fn }));
                }
                let (child, create_result) = create_child(locked, create_fn)?;
                entry.insert(Arc::downgrade(&child));
                (child, create_result)
            }
        };

        security::fs_node_init_with_dentry(locked, current_task, &child)?;

        child.node.fs().did_create_dir_entry(&child);
        Ok((child, create_result))
    }
}

impl fmt::Debug for DirEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parents = vec![];
        let mut maybe_parent = self.parent();
        while let Some(parent) = maybe_parent {
            parents.push(parent.local_name().to_string());
            maybe_parent = parent.parent();
        }
        let mut builder = f.debug_struct("DirEntry");
        builder.field("id", &(self as *const DirEntry));
        builder.field("local_name", &self.local_name());
        if !parents.is_empty() {
            builder.field("parents", &parents);
        }
        builder.finish()
    }
}

struct RenameGuard<'a> {
    old_parent_guard: DirEntryLockedChildren<'a>,
    new_parent_guard: Option<DirEntryLockedChildren<'a>>,
}

impl<'a> RenameGuard<'a> {
    fn lock(old_parent: &'a DirEntryHandle, new_parent: &'a DirEntryHandle) -> Self {
        if Arc::ptr_eq(old_parent, new_parent) {
            Self { old_parent_guard: old_parent.lock_children(), new_parent_guard: None }
        } else {
            // Following gVisor, these locks are taken in ancestor-to-descendant order.
            // Moreover, if the node are not comparable, they are taken from smallest inode to
            // biggest.
            if new_parent.is_descendant_of(old_parent)
                || (!old_parent.is_descendant_of(new_parent)
                    && old_parent.node.node_id < new_parent.node.node_id)
            {
                let old_parent_guard = old_parent.lock_children();
                let new_parent_guard = new_parent.lock_children();
                Self { old_parent_guard, new_parent_guard: Some(new_parent_guard) }
            } else {
                let new_parent_guard = new_parent.lock_children();
                let old_parent_guard = old_parent.lock_children();
                Self { old_parent_guard, new_parent_guard: Some(new_parent_guard) }
            }
        }
    }

    fn old_parent(&mut self) -> &mut DirEntryLockedChildren<'a> {
        &mut self.old_parent_guard
    }

    fn new_parent(&mut self) -> &mut DirEntryLockedChildren<'a> {
        if let Some(new_guard) = self.new_parent_guard.as_mut() {
            new_guard
        } else {
            &mut self.old_parent_guard
        }
    }
}

/// The Drop trait for DirEntry removes the entry from the child list of the
/// parent entry, which means we cannot drop DirEntry objects while holding a
/// lock on the parent's child list.
impl Drop for DirEntry {
    fn drop(&mut self) {
        let maybe_parent = self.state.write().parent.take();
        if let Some(parent) = maybe_parent {
            parent.internal_remove_child(self);
        }
    }
}
