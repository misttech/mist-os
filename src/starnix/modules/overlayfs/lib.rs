// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "512"]

use once_cell::sync::OnceCell;
use rand::Rng;
use starnix_core::fs::tmpfs::{TmpFs, TmpfsDirectory};
use starnix_core::mm::memory::MemoryObject;
use starnix_core::task::{CurrentTask, Kernel};
use starnix_core::vfs::fs_args::MountParams;
use starnix_core::vfs::rw_queue::RwQueueReadGuard;
use starnix_core::vfs::{
    default_seek, emit_dotdot, fileops_impl_directory, fileops_impl_noop_sync,
    fileops_impl_seekable, AlreadyLockedAppendLockStrategy, AppendLockGuard, CacheMode, DirEntry,
    DirEntryHandle, DirectoryEntryType, DirentSink, FallocMode, FileHandle, FileObject, FileOps,
    FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode, FsNodeHandle,
    FsNodeInfo, FsNodeOps, FsStr, FsString, InputBuffer, MountInfo, OutputBuffer, RenameFlags,
    SeekTarget, SymlinkTarget, UnlinkKind, ValueOrSize, VecInputBuffer, VecOutputBuffer, XattrOp,
};
use starnix_logging::{log_error, log_warn, track_stub};
use starnix_sync::{
    BeforeFsNodeAppend, FileOpsCore, FsNodeAppend, LockEqualOrBefore, Locked, RwLock,
    RwLockReadGuard, RwLockWriteGuard, Unlocked,
};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::{Errno, EEXIST, ENOENT};
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error, ino_t, off_t, statfs};
use std::collections::BTreeSet;
use std::sync::Arc;
use syncio::zxio_node_attr_has_t;

// Name and value for the xattr used to mark opaque directories in the upper FS.
// See https://docs.kernel.org/filesystems/overlayfs.html#whiteouts-and-opaque-directories
const OPAQUE_DIR_XATTR: &str = "trusted.overlay.opaque";
const OPAQUE_DIR_XATTR_VALUE: &str = "y";

#[derive(Clone)]
struct DirEntryInfo {
    name: FsString,
    inode_num: ino_t,
    entry_type: DirectoryEntryType,
}

type DirEntries = Vec<DirEntryInfo>;

#[derive(Default)]
struct DirentSinkAdapter {
    items: Vec<DirEntryInfo>,
    offset: off_t,
}

impl DirentSink for DirentSinkAdapter {
    fn add(
        &mut self,
        inode_num: ino_t,
        offset: off_t,
        entry_type: DirectoryEntryType,
        name: &FsStr,
    ) -> Result<(), Errno> {
        if !DirEntry::is_reserved_name(name) {
            self.items.push(DirEntryInfo { name: name.to_owned(), inode_num, entry_type });
        }
        self.offset = offset;
        Ok(())
    }

    fn offset(&self) -> off_t {
        self.offset
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum UpperCopyMode {
    MetadataOnly,
    CopyAll,
}

/// An `DirEntry` associated with the mount options. This is required because OverlayFs mostly
/// works at the `DirEntry` level (mounts on the lower, upper and work directories are ignored),
/// but operation must still depend on mount options.
#[derive(Clone)]
struct ActiveEntry {
    entry: DirEntryHandle,
    mount: MountInfo,
}

impl ActiveEntry {
    fn mapper<'a>(entry: &'a ActiveEntry) -> impl Fn(DirEntryHandle) -> ActiveEntry + 'a {
        |dir_entry| ActiveEntry { entry: dir_entry, mount: entry.mount.clone() }
    }

    fn entry(&self) -> &DirEntryHandle {
        &self.entry
    }

    fn mount(&self) -> &MountInfo {
        &self.mount
    }

    fn component_lookup<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<Self, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.entry()
            .component_lookup(locked, current_task, self.mount(), name)
            .map(ActiveEntry::mapper(self))
    }

    fn create_entry<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        create_node_fn: impl FnOnce(
            &mut Locked<'_, L>,
            &FsNodeHandle,
            &MountInfo,
            &FsStr,
        ) -> Result<FsNodeHandle, Errno>,
    ) -> Result<Self, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.entry()
            .create_entry(locked, current_task, self.mount(), name, create_node_fn)
            .map(ActiveEntry::mapper(self))
    }

    /// Sets an xattr to mark the directory referenced by `entry` as opaque. Directories that are
    /// marked as opaque in the upper FS are not merged with the corresponding directories in the
    /// lower FS.
    fn set_opaque_xattr<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.entry().node.set_xattr(
            locked,
            current_task,
            self.mount(),
            OPAQUE_DIR_XATTR.into(),
            OPAQUE_DIR_XATTR_VALUE.into(),
            XattrOp::Set,
        )
    }

    /// Checks if the `entry` is marked as opaque.
    fn is_opaque_node<L>(&self, locked: &mut Locked<'_, L>, current_task: &CurrentTask) -> bool
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        match self.entry().node.get_xattr(
            locked,
            current_task,
            self.mount(),
            OPAQUE_DIR_XATTR.into(),
            OPAQUE_DIR_XATTR_VALUE.len(),
        ) {
            Ok(ValueOrSize::Value(v)) if v == OPAQUE_DIR_XATTR_VALUE => true,
            _ => false,
        }
    }

    /// Creates a "whiteout" entry in the directory called `name`. Whiteouts are created by
    /// overlayfs to denote files and directories that were removed and should not be listed in the
    /// directory. This is necessary because we cannot remove entries from the lower FS.
    fn create_whiteout<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<ActiveEntry, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.create_entry(locked, current_task, name, |locked, dir, mount, name| {
            dir.mknod(
                locked,
                current_task,
                mount,
                name,
                FileMode::IFCHR,
                DeviceType::NONE,
                FsCred::root(),
            )
        })
    }

    /// Returns `true` if this is a "whiteout".
    fn is_whiteout(&self) -> bool {
        let info = self.entry().node.info();
        info.mode.is_chr() && info.rdev == DeviceType::NONE
    }

    /// Checks whether the child of this entry represented by `info` is a "whiteout".
    ///
    /// Only looks up the corresponding `DirEntry` when necessary.
    fn is_whiteout_child<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        info: &DirEntryInfo,
    ) -> Result<bool, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        // We need to lookup the node only if the file is a char device.
        if info.entry_type != DirectoryEntryType::CHR {
            return Ok(false);
        }
        let entry = self.component_lookup(locked, current_task, info.name.as_ref())?;
        Ok(entry.is_whiteout())
    }

    fn read_dir_entries<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<Vec<DirEntryInfo>, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut sink = DirentSinkAdapter::default();
        self.entry().open_anonymous(locked, current_task, OpenFlags::DIRECTORY)?.readdir(
            locked,
            current_task,
            &mut sink,
        )?;
        Ok(sink.items)
    }
}

struct OverlayNode {
    stack: Arc<OverlayStack>,

    // Corresponding `DirEntries` in the lower and the upper filesystems. At least one must be
    // set. Note that we don't care about `NamespaceNode`: overlayfs overlays filesystems
    // (i.e. not namespace subtrees). These directories may not be mounted anywhere.
    // `upper` may be created dynamically whenever write access is required.
    upper: OnceCell<ActiveEntry>,
    lower: Option<ActiveEntry>,

    // `prepare_to_unlink()` may mark `upper` as opaque. In that case we want to skip merging
    // with `lower` in `readdir()`.
    upper_is_opaque: OnceCell<()>,

    parent: Option<Arc<OverlayNode>>,
}

impl OverlayNode {
    fn new(
        stack: Arc<OverlayStack>,
        lower: Option<ActiveEntry>,
        upper: Option<ActiveEntry>,
        parent: Option<Arc<OverlayNode>>,
    ) -> Arc<Self> {
        assert!(upper.is_some() || parent.is_some());

        let upper = match upper {
            Some(entry) => OnceCell::with_value(entry),
            None => OnceCell::new(),
        };

        Arc::new(OverlayNode { stack, upper, lower, upper_is_opaque: OnceCell::new(), parent })
    }

    fn from_fs_node(node: &FsNodeHandle) -> Result<&Arc<Self>, Errno> {
        Ok(&node.downcast_ops::<OverlayNodeOps>().ok_or_else(|| errno!(EIO))?.node)
    }

    fn main_entry(&self) -> &ActiveEntry {
        self.upper
            .get()
            .or_else(|| self.lower.as_ref())
            .expect("Expected either upper or lower node")
    }

    fn init_fs_node_for_child(
        self: &Arc<OverlayNode>,
        current_task: &CurrentTask,
        node: &FsNode,
        lower: Option<ActiveEntry>,
        upper: Option<ActiveEntry>,
    ) -> FsNodeHandle {
        let entry = upper.as_ref().or(lower.as_ref()).expect("expect either lower or upper node");
        let info = entry.entry().node.info().clone();

        // Parent may be needed to initialize `upper`. We don't need to pass it if we have `upper`.
        let parent = if upper.is_some() { None } else { Some(self.clone()) };

        let overlay_node =
            OverlayNodeOps { node: OverlayNode::new(self.stack.clone(), lower, upper, parent) };
        FsNode::new_uncached(current_task, overlay_node, &node.fs(), info.ino, info)
    }

    /// If the file is currently in the lower FS, then promote it to the upper FS. No-op if the
    /// file is already in the upper FS.
    fn ensure_upper<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<&ActiveEntry, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.ensure_upper_maybe_copy(locked, current_task, UpperCopyMode::CopyAll)
    }

    /// Same as `ensure_upper()`, but allows to skip copying of the file content.
    fn ensure_upper_maybe_copy<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        copy_mode: UpperCopyMode,
    ) -> Result<&ActiveEntry, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.upper.get_or_try_init(|| {
            let lower = self.lower.as_ref().expect("lower is expected when upper is missing");
            let parent = self.parent.as_ref().expect("Parent is expected when upper is missing");
            let parent_upper = parent.ensure_upper(locked, current_task)?;
            let name = lower.entry().local_name();
            let info = {
                let info = lower.entry().node.info();
                info.clone()
            };
            let cred = info.cred();

            let res = if info.mode.is_lnk() {
                let link_target = lower.entry().node.readlink(current_task)?;
                let link_path = match &link_target {
                    SymlinkTarget::Node(_) => return error!(EIO),
                    SymlinkTarget::Path(path) => path,
                };
                parent_upper.create_entry(
                    locked,
                    current_task,
                    name.as_ref(),
                    |locked, dir, mount, name| {
                        dir.create_symlink(
                            locked,
                            current_task,
                            mount,
                            name,
                            link_path.as_ref(),
                            info.cred(),
                        )
                    },
                )
            } else if info.mode.is_reg() && copy_mode == UpperCopyMode::CopyAll {
                // Regular files need to be copied from lower FS to upper FS.
                self.stack.create_upper_entry(
                    locked,
                    current_task,
                    parent_upper,
                    name.as_ref(),
                    |locked, dir, name| {
                        dir.create_entry(
                            locked,
                            current_task,
                            name,
                            |locked, dir_node, mount, name| {
                                dir_node.mknod(
                                    locked,
                                    current_task,
                                    mount,
                                    name,
                                    info.mode,
                                    DeviceType::NONE,
                                    cred,
                                )
                            },
                        )
                    },
                    |locked, entry| copy_file_content(locked, current_task, lower, &entry),
                )
            } else {
                // TODO(sergeyu): create_node() checks access, but we don't need that here.
                parent_upper.create_entry(
                    locked,
                    current_task,
                    name.as_ref(),
                    |locked, dir, mount, name| {
                        dir.mknod(locked, current_task, mount, name, info.mode, info.rdev, cred)
                    },
                )
            };

            track_stub!(TODO("https://fxbug.dev/322874151"), "overlayfs copy xattrs");
            res
        })
    }

    /// Checks if this node exists in the lower FS.
    fn has_lower(&self) -> bool {
        self.lower.is_some()
    }

    /// Check that an item isn't present in the lower FS.
    fn lower_entry_exists<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<bool, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        match &self.lower {
            Some(lower) => match lower.component_lookup(locked, current_task, name) {
                Ok(entry) => Ok(!entry.is_whiteout()),
                Err(err) if err.code == ENOENT => Ok(false),
                Err(err) => Err(err),
            },
            None => Ok(false),
        }
    }

    /// Helper used to create a new entry in the directory. It first checks that the target node
    /// doesn't exist. Then `do_create` is called to create the new node in the work dir, which
    /// is then moved to the target dir in the upper file system.
    ///
    /// It's assumed that the calling `DirEntry` has the current directory locked, so it is not
    /// supposed to change while this method is executed. Note that OveralayFS doesn't handle
    /// the case when the underlying file systems are changed directly, but that restriction
    /// is not enforced.
    fn create_entry<F, L>(
        self: &Arc<OverlayNode>,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        do_create: F,
    ) -> Result<ActiveEntry, Errno>
    where
        F: Fn(&mut Locked<'_, L>, &ActiveEntry, &FsStr) -> Result<ActiveEntry, Errno>,
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let upper = self.ensure_upper(locked, current_task)?;

        match upper.component_lookup(locked, current_task, name) {
            Ok(existing) => {
                // If there is an entry in the upper dir, then it must be a whiteout.
                if !existing.is_whiteout() {
                    return error!(EEXIST);
                }
            }

            Err(e) if e.code == ENOENT => {
                // If we don't have the entry in the upper fs, then check lower.
                if self.lower_entry_exists(locked, current_task, name)? {
                    return error!(EEXIST);
                }
            }
            Err(e) => return Err(e),
        };

        self.stack.create_upper_entry(
            locked,
            current_task,
            upper,
            name,
            |locked, entry, fs| do_create(locked, entry, fs),
            |_, _entry| Ok(()),
        )
    }

    /// An overlay directory may appear empty when the corresponding upper dir isn't empty:
    /// it may contain a number of whiteout entries. In that case the whiteouts need to be
    /// unlinked before the upper directory can be unlinked as well.
    /// `prepare_to_unlink()` checks that the directory doesn't contain anything other
    /// than whiteouts and if that is the case then it unlinks all of them.
    fn prepare_to_unlink<L>(
        self: &Arc<OverlayNode>,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if self.main_entry().entry().node.is_dir() {
            let mut lower_entries = BTreeSet::new();
            if let Some(dir) = &self.lower {
                for item in dir.read_dir_entries(locked, current_task)?.drain(..) {
                    if !dir.is_whiteout_child(locked, current_task, &item)? {
                        lower_entries.insert(item.name);
                    }
                }
            }

            if let Some(dir) = self.upper.get() {
                let mut to_remove = Vec::<FsString>::new();
                for item in dir.read_dir_entries(locked, current_task)?.drain(..) {
                    if !dir.is_whiteout_child(locked, current_task, &item)? {
                        return error!(ENOTEMPTY);
                    }
                    lower_entries.remove(&item.name);
                    to_remove.push(item.name);
                }

                if !lower_entries.is_empty() {
                    return error!(ENOTEMPTY);
                }

                // Mark the directory as opaque. Children can be removed after this.
                dir.set_opaque_xattr(locked, current_task)?;
                let _ = self.upper_is_opaque.set(());

                // Finally, remove the children.
                for name in to_remove.iter() {
                    dir.entry().unlink(
                        locked,
                        current_task,
                        dir.mount(),
                        name.as_ref(),
                        UnlinkKind::NonDirectory,
                        false,
                    )?;
                }
            }
        }

        Ok(())
    }
}

struct OverlayNodeOps {
    node: Arc<OverlayNode>,
}

impl FsNodeOps for OverlayNodeOps {
    fn create_file_ops(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        if flags.can_write() {
            // Only upper FS can be writable.
            let copy_mode = if flags.contains(OpenFlags::TRUNC) {
                UpperCopyMode::MetadataOnly
            } else {
                UpperCopyMode::CopyAll
            };
            self.node.ensure_upper_maybe_copy(locked, current_task, copy_mode)?;
        }

        let ops: Box<dyn FileOps> = if node.is_dir() {
            Box::new(OverlayDirectory { node: self.node.clone(), dir_entries: Default::default() })
        } else {
            let state = match (self.node.upper.get(), &self.node.lower) {
                (Some(upper), _) => OverlayFileState::Upper(upper.entry().open_anonymous(
                    locked,
                    current_task,
                    flags,
                )?),
                (None, Some(lower)) => OverlayFileState::Lower(lower.entry().open_anonymous(
                    locked,
                    current_task,
                    flags,
                )?),
                _ => panic!("Expected either upper or lower node"),
            };

            Box::new(OverlayFile { node: self.node.clone(), flags, state: RwLock::new(state) })
        };

        Ok(ops)
    }

    fn lookup(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let resolve_child = |locked: &mut Locked<'_, FileOpsCore>,
                             dir_opt: Option<&ActiveEntry>| {
            // TODO(sergeyu): lookup() checks access, but we don't need that here.
            dir_opt
                .as_ref()
                .map(|dir| match dir.component_lookup(locked, current_task, name) {
                    Ok(entry) => Some(Ok(entry)),
                    Err(e) if e.code == ENOENT => None,
                    Err(e) => Some(Err(e)),
                })
                .flatten()
                .transpose()
        };

        let upper: Option<ActiveEntry> = resolve_child(locked, self.node.upper.get())?;

        let (upper_is_dir, upper_is_opaque) = match &upper {
            Some(upper) if upper.is_whiteout() => return error!(ENOENT),
            Some(upper) => {
                let is_dir = upper.entry().node.is_dir();
                let is_opaque = !is_dir || upper.is_opaque_node(locked, current_task);
                (is_dir, is_opaque)
            }
            None => (false, false),
        };

        let parent_upper_is_opaque = self.node.upper_is_opaque.get().is_some();

        // We don't need to resolve the lower node if we have an opaque node in the upper dir.
        let lookup_lower = !parent_upper_is_opaque && !upper_is_opaque;
        let lower: Option<ActiveEntry> = if lookup_lower {
            match resolve_child(locked, self.node.lower.as_ref())? {
                // If the upper node is a directory and the lower isn't then ignore the lower node.
                Some(lower) if upper_is_dir && !lower.entry().node.is_dir() => None,
                Some(lower) if lower.is_whiteout() => None,
                result => result,
            }
        } else {
            None
        };

        if upper.is_none() && lower.is_none() {
            return error!(ENOENT);
        }

        Ok(self.node.init_fs_node_for_child(current_task, node, lower, upper))
    }

    fn mknod(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let new_upper_node =
            self.node.create_entry(locked, current_task, name, |locked, dir, temp_name| {
                dir.create_entry(
                    locked,
                    current_task,
                    temp_name,
                    |locked, dir_node, mount, name| {
                        dir_node.mknod(locked, current_task, mount, name, mode, dev, owner.clone())
                    },
                )
            })?;
        Ok(self.node.init_fs_node_for_child(current_task, node, None, Some(new_upper_node)))
    }

    fn mkdir(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let new_upper_node =
            self.node.create_entry(locked, current_task, name, |locked, dir, temp_name| {
                let entry = dir.create_entry(
                    locked,
                    current_task,
                    temp_name,
                    |locked, dir_node, mount, name| {
                        dir_node.mknod(
                            locked,
                            current_task,
                            mount,
                            name,
                            mode,
                            DeviceType::NONE,
                            owner.clone(),
                        )
                    },
                )?;

                // Set opaque attribute to ensure the new directory is not merged with lower.
                entry.set_opaque_xattr(locked, current_task)?;

                Ok(entry)
            })?;

        Ok(self.node.init_fs_node_for_child(current_task, node, None, Some(new_upper_node)))
    }

    fn create_symlink(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        target: &FsStr,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let new_upper_node =
            self.node.create_entry(locked, current_task, name, |locked, dir, temp_name| {
                dir.create_entry(
                    locked,
                    current_task,
                    temp_name,
                    |locked, dir_node, mount, name| {
                        dir_node.create_symlink(
                            locked,
                            current_task,
                            mount,
                            name,
                            target,
                            owner.clone(),
                        )
                    },
                )
            })?;
        Ok(self.node.init_fs_node_for_child(current_task, node, None, Some(new_upper_node)))
    }

    fn readlink(&self, _node: &FsNode, current_task: &CurrentTask) -> Result<SymlinkTarget, Errno> {
        self.node.main_entry().entry().node.readlink(current_task)
    }

    fn link(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        let child_overlay = OverlayNode::from_fs_node(child)?;
        let upper_child = child_overlay.ensure_upper(locked, current_task)?;
        self.node.create_entry(locked, current_task, name, |locked, dir, temp_name| {
            dir.create_entry(locked, current_task, temp_name, |locked, dir_node, mount, name| {
                dir_node.link(locked, current_task, mount, name, &upper_child.entry().node)
            })
        })?;
        Ok(())
    }

    fn unlink(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        let upper = self.node.ensure_upper(locked, current_task)?;
        let child_overlay = OverlayNode::from_fs_node(child)?;
        child_overlay.prepare_to_unlink(locked, current_task)?;

        let need_whiteout = self.node.lower_entry_exists(locked, current_task, name)?;
        if need_whiteout {
            self.node.stack.create_upper_entry(
                locked,
                current_task,
                &upper,
                &name,
                |locked, work, name| work.create_whiteout(locked, current_task, name),
                |_, _entry| Ok(()),
            )?;
        } else if let Some(child_upper) = child_overlay.upper.get() {
            let kind = if child_upper.entry().node.is_dir() {
                UnlinkKind::Directory
            } else {
                UnlinkKind::NonDirectory
            };
            upper.entry().unlink(locked, current_task, upper.mount(), name, kind, false)?
        }

        Ok(())
    }

    fn fetch_and_refresh_info<'a>(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        let real_info = self
            .node
            .main_entry()
            .entry()
            .node
            .fetch_and_refresh_info(locked, current_task)?
            .clone();
        let mut lock = info.write();
        *lock = real_info;
        Ok(RwLockWriteGuard::downgrade(lock))
    }

    fn update_attributes(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        current_task: &CurrentTask,
        new_info: &FsNodeInfo,
        has: zxio_node_attr_has_t,
    ) -> Result<(), Errno> {
        let upper = self.node.ensure_upper(locked, current_task)?.entry();
        upper.node.update_attributes(locked, current_task, |info| {
            if has.modification_time {
                info.time_modify = new_info.time_modify;
            }
            if has.access_time {
                info.time_access = new_info.time_access;
            }
            if has.mode {
                info.mode = new_info.mode;
            }
            if has.uid {
                info.uid = new_info.uid;
            }
            if has.gid {
                info.gid = new_info.gid;
            }
            if has.rdev {
                info.rdev = new_info.rdev;
            }
            Ok(())
        })
    }

    fn append_lock_read<'a>(
        &'a self,
        locked: &'a mut Locked<'a, BeforeFsNodeAppend>,
        _node: &'a FsNode,
        current_task: &CurrentTask,
    ) -> Result<(RwQueueReadGuard<'a, FsNodeAppend>, Locked<'a, FsNodeAppend>), Errno> {
        let upper_node = self.node.ensure_upper(locked, current_task)?.entry.node.as_ref();
        upper_node.ops().append_lock_read(locked, upper_node, current_task)
    }

    fn truncate(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        guard: &AppendLockGuard<'_>,
        _node: &FsNode,
        current_task: &CurrentTask,
        length: u64,
    ) -> Result<(), Errno> {
        let upper = self.node.ensure_upper(locked, current_task)?;

        upper.entry().node.truncate_with_strategy(
            locked,
            AlreadyLockedAppendLockStrategy::new(guard),
            current_task,
            upper.mount(),
            length,
        )
    }

    fn allocate(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        guard: &AppendLockGuard<'_>,
        _node: &FsNode,
        current_task: &CurrentTask,
        mode: FallocMode,
        offset: u64,
        length: u64,
    ) -> Result<(), Errno> {
        self.node.ensure_upper(locked, current_task)?.entry().node.fallocate_with_strategy(
            locked,
            AlreadyLockedAppendLockStrategy::new(guard),
            current_task,
            mode,
            offset,
            length,
        )
    }
}
struct OverlayDirectory {
    node: Arc<OverlayNode>,
    dir_entries: RwLock<DirEntries>,
}

impl OverlayDirectory {
    fn refresh_dir_entries<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut entries = DirEntries::new();

        let upper_is_opaque = self.node.upper_is_opaque.get().is_some();
        let merge_with_lower = self.node.lower.is_some() && !upper_is_opaque;

        // First enumerate entries in the upper dir. Then enumerate the lower dir and add only
        // items that are not present in the upper.
        let mut upper_set = BTreeSet::new();
        if let Some(dir) = self.node.upper.get() {
            for item in dir.read_dir_entries(locked, current_task)?.drain(..) {
                // Fill `upper_set` only if we will need it later.
                if merge_with_lower {
                    upper_set.insert(item.name.clone());
                }
                if !dir.is_whiteout_child(locked, current_task, &item)? {
                    entries.push(item);
                }
            }
        }

        if merge_with_lower {
            if let Some(dir) = &self.node.lower {
                for item in dir.read_dir_entries(locked, current_task)?.drain(..) {
                    if !upper_set.contains(&item.name)
                        && !dir.is_whiteout_child(locked, current_task, &item)?
                    {
                        entries.push(item);
                    }
                }
            }
        }

        *self.dir_entries.write() = entries;

        Ok(())
    }
}

impl FileOps for OverlayDirectory {
    fileops_impl_directory!();
    fileops_impl_noop_sync!();

    fn seek(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        default_seek(current_offset, target, |_| error!(EINVAL))
    }

    fn readdir(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        if sink.offset() == 0 {
            self.refresh_dir_entries(locked, current_task)?;
        }

        emit_dotdot(file, sink)?;

        for item in self.dir_entries.read().iter().skip(sink.offset() as usize - 2) {
            sink.add(item.inode_num, sink.offset() + 1, item.entry_type, item.name.as_ref())?;
        }

        Ok(())
    }
}

enum OverlayFileState {
    Lower(FileHandle),
    Upper(FileHandle),
}

impl OverlayFileState {
    fn file(&self) -> &FileHandle {
        match self {
            Self::Lower(f) | Self::Upper(f) => f,
        }
    }
}

struct OverlayFile {
    node: Arc<OverlayNode>,
    flags: OpenFlags,
    state: RwLock<OverlayFileState>,
}

impl FileOps for OverlayFile {
    fileops_impl_seekable!();

    fn read(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let mut state = self.state.read();

        // Check if the file was promoted to the upper FS. In that case we need to reopen it from
        // there.
        if let Some(upper) = self.node.upper.get() {
            if matches!(*state, OverlayFileState::Lower(_)) {
                std::mem::drop(state);

                {
                    let mut write_state = self.state.write();

                    // TODO(mariagl): don't hold write_state while calling open_anonymous.
                    // It may call back into read(), causing lock order inversion.
                    *write_state = OverlayFileState::Upper(upper.entry().open_anonymous(
                        locked,
                        current_task,
                        self.flags,
                    )?);
                }
                state = self.state.read();
            }
        }

        // TODO(mariagl): Drop state here
        state.file().read_at(locked, current_task, offset, data)
    }

    fn write(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let state = self.state.read();
        let file = match &*state {
            OverlayFileState::Upper(f) => f.clone(),

            // `write()` should be called only for files that were opened for write, and that
            // required the file to be promoted to the upper FS.
            OverlayFileState::Lower(_) => panic!("write() called for a lower FS file."),
        };
        std::mem::drop(state);
        file.write_at(locked, current_task, offset, data)
    }

    fn sync(&self, _file: &FileObject, current_task: &CurrentTask) -> Result<(), Errno> {
        self.state.read().file().sync(current_task)
    }

    fn get_memory(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        length: Option<usize>,
        prot: starnix_core::mm::ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno> {
        // Not that the VMO returned here will not updated if the file is promoted to upper FS
        // later. This is consistent with OveralyFS behavior on Linux, see
        // https://docs.kernel.org/filesystems/overlayfs.html#non-standard-behavior .
        self.state.read().file().get_memory(locked, current_task, length, prot)
    }
}

pub fn new_overlay_fs(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    OverlayStack::new_fs(locked, current_task, options)
}

pub struct OverlayStack {
    // Keep references to the underlying file systems to ensure they outlive `overlayfs` since
    // they may be unmounted before overlayfs.
    #[allow(unused)]
    lower_fs: FileSystemHandle,
    upper_fs: FileSystemHandle,

    work: ActiveEntry,
}

impl OverlayStack {
    fn new_fs(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        match options.params.get("redirect_dir".as_bytes()) {
            None => (),
            Some(o) if o == "off" => (),
            Some(_) => {
                track_stub!(TODO("https://fxbug.dev/322874205"), "overlayfs redirect_dir");
                return error!(ENOTSUP);
            }
        }

        let lower = resolve_dir_param(locked, current_task, &options.params, "lowerdir".into())?;
        let upper = resolve_dir_param(locked, current_task, &options.params, "upperdir".into())?;
        let work = resolve_dir_param(locked, current_task, &options.params, "workdir".into())?;

        let lower_fs = lower.entry().node.fs().clone();
        let upper_fs = upper.entry().node.fs().clone();

        if !Arc::ptr_eq(&upper_fs, &work.entry().node.fs()) {
            log_error!("overlayfs: upperdir and workdir must be on the same FS");
            return error!(EINVAL);
        }

        let stack = Arc::new(OverlayStack { lower_fs, upper_fs, work });
        let root_node = OverlayNode::new(stack.clone(), Some(lower), Some(upper), None);
        let fs = FileSystem::new(
            current_task.kernel(),
            CacheMode::Uncached,
            OverlayFs { stack },
            options,
        )?;
        fs.set_root(OverlayNodeOps { node: root_node });
        Ok(fs)
    }

    /// Given a filesystem, wraps it in a tmpfs-backed writable overlayfs.
    pub fn wrap_fs_in_writable_layer(
        kernel: &Arc<Kernel>,
        rootfs: FileSystemHandle,
    ) -> Result<FileSystemHandle, Errno> {
        let lower = ActiveEntry { entry: rootfs.root().clone(), mount: MountInfo::detached() };

        // Create upper and work directories in an invisible tmpfs.
        let invisible_tmp = TmpFs::new_fs(kernel);
        let upper = ActiveEntry {
            entry: invisible_tmp.insert_node(FsNode::new_root(TmpfsDirectory::new())),
            mount: MountInfo::detached(),
        };
        let work = ActiveEntry {
            entry: invisible_tmp.insert_node(FsNode::new_root(TmpfsDirectory::new())),
            mount: MountInfo::detached(),
        };

        let lower_fs = rootfs.clone();
        let upper_fs = invisible_tmp.clone();

        let stack = Arc::new(OverlayStack { lower_fs, upper_fs, work });
        let root_node = OverlayNode::new(stack.clone(), Some(lower), Some(upper), None);
        let fs = FileSystem::new(
            kernel,
            CacheMode::Uncached,
            OverlayFs { stack },
            FileSystemOptions::default(),
        )?;
        fs.set_root(OverlayNodeOps { node: root_node });
        Ok(fs)
    }

    // Helper used to create new entry called `name` in `target_dir` in the upper FS.
    // 1. Calls `try_create` to create a new entry in `work`. It is called repeateadly with a
    //    new name until it returns any result other than `EEXIST`.
    // 2. `do_init` is called to initilize the contents and the attributes of the new entry, etc.
    // 3. The new entry is moved to `target_dir`. If there is an existing entry called `name` in
    //    `target_dir` then it's replaced with the new entry.
    // The temp file is cleared from the work dir if either of the last two steps fails.
    fn create_upper_entry<FCreate, FInit, L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        target_dir: &ActiveEntry,
        name: &FsStr,
        try_create: FCreate,
        do_init: FInit,
    ) -> Result<ActiveEntry, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
        FCreate: Fn(&mut Locked<'_, L>, &ActiveEntry, &FsStr) -> Result<ActiveEntry, Errno>,
        FInit: FnOnce(&mut Locked<'_, L>, &ActiveEntry) -> Result<(), Errno>,
    {
        let mut rng = rand::thread_rng();
        let (temp_name, entry) = loop {
            let x: u64 = rng.gen();
            let temp_name = FsString::from(format!("tmp{:x}", x));
            match try_create(locked, &self.work, temp_name.as_ref()) {
                Err(err) if err.code == EEXIST => continue,
                Err(err) => return Err(err),
                Ok(entry) => break (temp_name, entry),
            }
        };

        do_init(locked, &entry)
            .and_then(|()| {
                DirEntry::rename(
                    locked,
                    current_task,
                    self.work.entry(),
                    self.work.mount(),
                    temp_name.as_ref(),
                    target_dir.entry(),
                    target_dir.mount(),
                    name,
                    RenameFlags::REPLACE_ANY,
                )
            })
            .map_err(|e| {
                // Remove the temp entry in case of a failure.
                self.work
                    .entry()
                    .unlink(
                        locked,
                        current_task,
                        self.work.mount(),
                        temp_name.as_ref(),
                        UnlinkKind::NonDirectory,
                        false,
                    )
                    .unwrap_or_else(|e| {
                        log_error!("Failed to cleanup work dir after an error: {}", e)
                    });
                e
            })?;

        Ok(entry)
    }
}

struct OverlayFs {
    stack: Arc<OverlayStack>,
}

impl FileSystemOps for OverlayFs {
    fn statfs(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        self.stack.upper_fs.statfs(locked, current_task)
    }

    fn name(&self) -> &'static FsStr {
        "overlay".into()
    }

    fn rename(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        current_task: &CurrentTask,
        old_parent: &FsNodeHandle,
        old_name: &FsStr,
        new_parent: &FsNodeHandle,
        new_name: &FsStr,
        renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        let renamed = OverlayNode::from_fs_node(renamed)?;
        if renamed.has_lower() && renamed.main_entry().entry().node.is_dir() {
            // Return EXDEV for directory renames. Potentially they may be handled with the
            // `redirect_dir` feature, but it's not implemented here yet.
            // See https://docs.kernel.org/filesystems/overlayfs.html#renaming-directories
            return error!(EXDEV);
        }
        renamed.ensure_upper(locked, current_task)?;

        let old_parent_overlay = OverlayNode::from_fs_node(old_parent)?;
        let old_parent_upper = old_parent_overlay.ensure_upper(locked, current_task)?;

        let new_parent_overlay = OverlayNode::from_fs_node(new_parent)?;
        let new_parent_upper = new_parent_overlay.ensure_upper(locked, current_task)?;

        let need_whiteout =
            old_parent_overlay.lower_entry_exists(locked, current_task, old_name)?;

        DirEntry::rename(
            locked,
            current_task,
            old_parent_upper.entry(),
            old_parent_upper.mount(),
            old_name,
            new_parent_upper.entry(),
            new_parent_upper.mount(),
            new_name,
            RenameFlags::REPLACE_ANY,
        )?;

        // If the old node existed in lower FS, then override it in the upper FS with a whiteout.
        if need_whiteout {
            match old_parent_upper.create_whiteout(locked, current_task, old_name) {
                Err(e) => log_warn!("overlayfs: failed to create whiteout for {old_name}: {e}"),
                Ok(_) => (),
            }
        }

        Ok(())
    }

    fn unmount(&self) {}
}

/// Helper used to resolve directories passed in mount options. The directory is resolved in the
/// namespace of the calling process, but only `DirEntry` is returned (detached from the
/// namespace). The corresponding file systems may be unmounted before overlayfs that uses them.
fn resolve_dir_param(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    params: &MountParams,
    name: &FsStr,
) -> Result<ActiveEntry, Errno> {
    let path = params.get(&**name).ok_or_else(|| {
        log_error!("overlayfs: {name} was not specified");
        errno!(EINVAL)
    })?;

    current_task
        .open_file(locked, path.as_ref(), OpenFlags::RDONLY | OpenFlags::DIRECTORY)
        .map(|f| ActiveEntry { entry: f.name.entry.clone(), mount: f.name.mount.clone() })
        .map_err(|e| {
            log_error!("overlayfs: Failed to lookup {path}: {}", e);
            e
        })
}

/// Copies file content from one file to another.
fn copy_file_content<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    from: &ActiveEntry,
    to: &ActiveEntry,
) -> Result<(), Errno>
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    let from_file = from.entry().open_anonymous(locked, current_task, OpenFlags::RDONLY)?;
    let to_file = to.entry().open_anonymous(locked, current_task, OpenFlags::WRONLY)?;

    const BUFFER_SIZE: usize = 4096;

    loop {
        // TODO(sergeyu): Reuse buffer between iterations.

        let mut output_buffer = VecOutputBuffer::new(BUFFER_SIZE);
        let bytes_read = from_file.read(locked, current_task, &mut output_buffer)?;
        if bytes_read == 0 {
            break;
        }

        let buffer: Vec<u8> = output_buffer.into();
        let mut input_buffer = VecInputBuffer::from(buffer);
        while input_buffer.available() > 0 {
            to_file.write(locked, current_task, &mut input_buffer)?;
        }
    }

    to_file.data_sync(current_task)?;

    Ok(())
}
