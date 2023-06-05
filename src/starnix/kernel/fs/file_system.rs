// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use linked_hash_map::LinkedHashMap;
use once_cell::sync::OnceCell;
use ref_cast::RefCast;
use smallvec::SmallVec;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use super::*;
use crate::lock::Mutex;
use crate::task::Kernel;
use crate::types::{as_any::AsAny, *};

const LRU_CAPACITY: usize = 32;

/// A file system that can be mounted in a namespace.
pub struct FileSystem {
    root: OnceCell<DirEntryHandle>,
    next_node_id: AtomicU64,
    ops: Box<dyn FileSystemOps>,

    /// The options specified when mounting the filesystem. Saved here for display in
    /// /proc/[pid]/mountinfo.
    pub options: FileSystemOptions,

    /// The device ID of this filesystem. Returned in the st_dev field when stating an inode in
    /// this filesystem.
    pub dev_id: DeviceType,

    /// A file-system global mutex to serialize rename operations.
    ///
    /// This mutex is useful because the invariants enforced during a rename
    /// operation involve many DirEntry objects. In the future, we might be
    /// able to remove this mutex, but we will need to think carefully about
    /// how rename operations can interleave.
    ///
    /// See DirEntry::rename.
    pub rename_mutex: Mutex<()>,

    /// The FsNode cache for this file system.
    ///
    /// When two directory entries are hard links to the same underlying inode,
    /// this cache lets us re-use the same FsNode object for both directory
    /// entries.
    ///
    /// Rather than calling FsNode::new directly, file systems should call
    /// FileSystem::get_or_create_node to see if the FsNode already exists in
    /// the cache.
    nodes: Mutex<HashMap<ino_t, Weak<FsNode>>>,

    /// DirEntryHandle cache for the filesystem. Holds strong references to DirEntry objects. For
    /// filesystems with permanent entries, this will hold a strong reference to every node to make
    /// sure it doesn't get freed without being explicitly unlinked. Otherwise, entries are
    /// maintained in an LRU cache.
    entries: Entries,

    /// Hack meant to stand in for the fs_use_trans selinux feature. If set, this value will be set
    /// as the selinux label on any newly created inodes in the filesystem.
    pub selinux_context: OnceCell<FsString>,
}

#[derive(Clone, Debug, Default)]
pub struct FileSystemOptions {
    /// The source string passed as the first argument to mount(), e.g. a block device.
    pub source: FsString,
    /// Flags kept per-superblock, i.e. included in MountFlags::STORED_ON_FILESYSTEM.
    pub flags: MountFlags,
    /// Filesystem options passed as the last argument to mount().
    pub params: FsString,
}

impl FileSystemOptions {
    pub fn source_for_display(&self) -> &FsStr {
        if self.source.is_empty() {
            return b"none";
        }
        &self.source
    }
}

enum Entries {
    Permanent(Mutex<HashSet<ArcKey<DirEntry>>>),
    Lru(Mutex<LinkedHashMap<ArcKey<DirEntry>, ()>>),
    Uncached,
}

pub enum CacheMode {
    /// Entries are pemanent, instead of a cache of the backing storage. An example is tmpfs: the
    /// DirEntry tree *is* the backing storage, as opposed to ext4, which uses the DirEntry tree as
    /// a cache and removes unused nodes from it.
    Permanent,
    /// Entries are cached.
    Cached,
    /// Entries are uncached. This can be appropriate in cases where it is difficult for the
    /// filesystem to keep the cache coherent: e.g. the /proc/<pid>/task directory.
    Uncached,
}

impl FileSystem {
    /// Create a new filesystem.
    pub fn new(
        kernel: &Kernel,
        cache_mode: CacheMode,
        ops: impl FileSystemOps,
        options: FileSystemOptions,
    ) -> FileSystemHandle {
        Arc::new(FileSystem {
            root: OnceCell::new(),
            next_node_id: AtomicU64::new(1),
            ops: Box::new(ops),
            options,
            dev_id: kernel.device_registry.write().next_anonymous_dev_id(),
            rename_mutex: Mutex::new(()),
            nodes: Mutex::new(HashMap::new()),
            entries: match cache_mode {
                CacheMode::Permanent => Entries::Permanent(Mutex::new(HashSet::new())),
                CacheMode::Cached => Entries::Lru(Mutex::new(LinkedHashMap::new())),
                CacheMode::Uncached => Entries::Uncached,
            },
            selinux_context: OnceCell::new(),
        })
    }

    pub fn set_root(self: &FileSystemHandle, root: impl FsNodeOps) {
        self.set_root_node(FsNode::new_root(root));
    }

    /// Set up the root of the filesystem. Must not be called more than once.
    pub fn set_root_node(self: &FileSystemHandle, mut root: FsNode) {
        if root.node_id == 0 {
            root.set_id(self.next_node_id());
        }
        root.set_fs(self);
        let root_node = Arc::new(root);
        self.nodes.lock().insert(root_node.node_id, Arc::downgrade(&root_node));
        let root = DirEntry::new(root_node, None, FsString::new());
        assert!(self.root.set(root).is_ok(), "FileSystem::set_root can't be called more than once");
    }

    pub fn has_permanent_entries(&self) -> bool {
        matches!(self.entries, Entries::Permanent(_))
    }

    /// The root directory entry of this file system.
    ///
    /// Panics if this file system does not have a root directory.
    pub fn root(&self) -> &DirEntryHandle {
        self.root.get().unwrap()
    }

    /// Prepare a node for insertion in the node cache.
    ///
    /// Currently, apply the required selinux context if the selinux workaround is enabled on this
    /// filesystem.
    fn prepare_node_for_insertion(&self, node: &Arc<FsNode>) -> Weak<FsNode> {
        if let Some(label) = self.selinux_context.get() {
            let _ = node.ops().set_xattr(node, b"security.selinux", label, XattrOp::Create);
        }
        Arc::downgrade(node)
    }

    /// Get or create an FsNode for this file system.
    ///
    /// If node_id is Some, then this function checks the node cache to
    /// determine whether this node is already open. If so, the function
    /// returns the existing FsNode. If not, the function calls the given
    /// create_fn function to create the FsNode.
    ///
    /// If node_id is None, then this function assigns a new identifier number
    /// and calls the given create_fn function to create the FsNode with the
    /// assigned number.
    ///
    /// Returns Err only if create_fn returns Err.
    pub fn get_or_create_node<F>(
        &self,
        node_id: Option<ino_t>,
        create_fn: F,
    ) -> Result<FsNodeHandle, Errno>
    where
        F: FnOnce(ino_t) -> Result<FsNodeHandle, Errno>,
    {
        let node_id = node_id.unwrap_or_else(|| self.next_node_id());
        let mut nodes = self.nodes.lock();
        match nodes.entry(node_id) {
            Entry::Vacant(entry) => {
                let node = create_fn(node_id)?;
                entry.insert(self.prepare_node_for_insertion(&node));
                Ok(node)
            }
            Entry::Occupied(mut entry) => {
                if let Some(node) = entry.get().upgrade() {
                    return Ok(node);
                }
                let node = create_fn(node_id)?;
                entry.insert(self.prepare_node_for_insertion(&node));
                Ok(node)
            }
        }
    }

    /// File systems that produce their own IDs for nodes should invoke this
    /// function. The ones who leave to this object to assign the IDs should
    /// call |create_node|.
    pub fn create_node_with_id(
        self: &Arc<Self>,
        ops: Box<dyn FsNodeOps>,
        id: ino_t,
        info: FsNodeInfo,
    ) -> FsNodeHandle {
        let node = FsNode::new_uncached(ops, self, id, info);
        self.nodes.lock().insert(node.node_id, self.prepare_node_for_insertion(&node));
        node
    }

    pub fn create_node_box(
        self: &Arc<Self>,
        ops: Box<dyn FsNodeOps>,
        info: impl FnOnce(ino_t) -> FsNodeInfo,
    ) -> FsNodeHandle {
        let node_id = self.next_node_id();
        self.create_node_with_id(ops, node_id, info(node_id))
    }

    pub fn create_node(
        self: &Arc<Self>,
        ops: impl FsNodeOps,
        info: impl FnOnce(ino_t) -> FsNodeInfo,
    ) -> FsNodeHandle {
        self.create_node_box(Box::new(ops), info)
    }

    /// Remove the given FsNode from the node cache.
    ///
    /// Called from the Drop trait of FsNode.
    pub fn remove_node(&self, node: &mut FsNode) {
        let mut nodes = self.nodes.lock();
        if let Some(weak_node) = nodes.get(&node.node_id) {
            if std::ptr::eq(weak_node.as_ptr(), node) {
                nodes.remove(&node.node_id);
            }
        }
    }

    pub fn next_node_id(&self) -> ino_t {
        assert!(!self.ops.generate_node_ids());
        self.next_node_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Move |renamed| that is at |old_name| in |old_parent| to |new_name| in |new_parent|
    /// replacing |replaced|.
    /// If |replaced| exists and is a directory, this function must check that |renamed| is n
    /// directory and that |replaced| is empty.
    pub fn rename(
        &self,
        old_parent: &FsNodeHandle,
        old_name: &FsStr,
        new_parent: &FsNodeHandle,
        new_name: &FsStr,
        renamed: &FsNodeHandle,
        replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        self.ops.rename(self, old_parent, old_name, new_parent, new_name, renamed, replaced)
    }

    /// Called each time the filesystem is unmounted.
    pub fn unmount(&self) {
        self.ops.unmount();
    }

    /// Returns the `statfs` for this filesystem.
    ///
    /// Each `FileSystemOps` impl is expected to override this to return the specific statfs for
    /// the filesystem.
    ///
    /// Returns `ENOSYS` if the `FileSystemOps` don't implement `stat`.
    pub fn statfs(&self) -> Result<statfs, Errno> {
        let mut stat = self.ops.statfs(self)?;
        if stat.f_frsize == 0 {
            stat.f_frsize = stat.f_bsize as i64;
        }
        Ok(stat)
    }

    pub fn did_create_dir_entry(&self, entry: &DirEntryHandle) {
        match &self.entries {
            Entries::Permanent(p) => {
                p.lock().insert(ArcKey(entry.clone()));
            }
            Entries::Lru(l) => {
                l.lock().insert(ArcKey(entry.clone()), ());
            }
            Entries::Uncached => {}
        }
    }

    pub fn will_destroy_dir_entry(&self, entry: &DirEntryHandle) {
        match &self.entries {
            Entries::Permanent(p) => {
                p.lock().remove(ArcKey::ref_cast(entry));
            }
            Entries::Lru(l) => {
                l.lock().remove(ArcKey::ref_cast(entry));
            }
            Entries::Uncached => {}
        };
    }

    /// Informs the cache that the entry was used.
    pub fn did_access_dir_entry(&self, entry: &DirEntryHandle) {
        if let Entries::Lru(l) = &self.entries {
            l.lock().get_refresh(ArcKey::ref_cast(entry));
        }
    }

    /// Purges old entries from the cache. This is done as a separate step to avoid potential
    /// deadlocks that could occur if done at admission time (where locks might be held that are
    /// required when dropping old entries). This should be called after any new entries are
    /// admitted with no locks held that might be required for dropping entries.
    pub fn purge_old_entries(&self) {
        if let Entries::Lru(l) = &self.entries {
            let mut purged = SmallVec::<[DirEntryHandle; 4]>::new();
            {
                let mut l = l.lock();
                while l.len() > LRU_CAPACITY {
                    purged.push(l.pop_front().unwrap().0 .0);
                }
            }
            // Entries will get dropped here whilst we're not holding a lock.
            std::mem::drop(purged);
        }
    }

    /// Returns the `FileSystem`'s `FileSystemOps` as a `&T`, or `None` if the downcast fails.
    pub fn downcast_ops<T: 'static>(&self) -> Option<&T> {
        self.ops.as_ref().as_any().downcast_ref()
    }

    pub fn name(&self) -> &'static FsStr {
        self.ops.name()
    }
}

/// The filesystem-implementation-specific data for FileSystem.
pub trait FileSystemOps: AsAny + Send + Sync + 'static {
    /// Return information about this filesystem.
    ///
    /// A typical implementation looks like this:
    /// ```
    /// Ok(statfs::default(FILE_SYSTEM_MAGIC))
    /// ```
    /// or, if the filesystem wants to customize fields:
    /// ```
    /// Ok(statfs {
    ///     f_blocks: self.blocks,
    ///     ..statfs::default(FILE_SYSTEM_MAGIC)
    /// })
    /// ```
    fn statfs(&self, _fs: &FileSystem) -> Result<statfs, Errno>;

    fn name(&self) -> &'static FsStr;

    /// Whether this file system generates its own node IDs.
    fn generate_node_ids(&self) -> bool {
        false
    }

    /// Rename the given node.
    ///
    /// The node to be renamed is passed as "renamed". It currently has
    /// old_name in old_parent. After the rename operation, it should have
    /// new_name in new_parent.
    ///
    /// If new_parent already has a child named new_name, that node is passed as
    /// "replaced". In that case, both "renamed" and "replaced" will be
    /// directories and the rename operation should succeed only if "replaced"
    /// is empty. The VFS will check that there are no children of "replaced" in
    /// the DirEntry cache, but the implementation of this function is
    /// responsible for checking that there are no children of replaced that are
    /// known only to the file system implementation (e.g., present on-disk but
    /// not in the DirEntry cache).
    fn rename(
        &self,
        _fs: &FileSystem,
        _old_parent: &FsNodeHandle,
        _old_name: &FsStr,
        _new_parent: &FsNodeHandle,
        _new_name: &FsStr,
        _renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        error!(EROFS)
    }

    /// Called each time the filesystem is unmounted.
    fn unmount(&self) {}
}

pub type FileSystemHandle = Arc<FileSystem>;
