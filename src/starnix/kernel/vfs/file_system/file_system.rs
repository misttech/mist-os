// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::security;
use crate::task::{CurrentTask, Kernel};
use crate::vfs::fs_args::MountParams;
use crate::vfs::{
    DirEntry, DirEntryHandle, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, FsString,
    WeakFsNodeHandle,
};
use linked_hash_map::LinkedHashMap;
use ref_cast::RefCast;
use smallvec::SmallVec;
use starnix_lifecycle::AtomicU64Counter;
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked, Mutex};
use starnix_uapi::arc_key::ArcKey;
use starnix_uapi::as_any::AsAny;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::{error, ino_t, statfs};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, Weak};

pub const DEFAULT_LRU_CAPACITY: usize = 32;

/// A file system that can be mounted in a namespace.
pub struct FileSystem {
    kernel: Weak<Kernel>,
    root: OnceLock<DirEntryHandle>,
    next_node_id: AtomicU64Counter,
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
    nodes: Mutex<HashMap<ino_t, WeakFsNodeHandle>>,

    /// DirEntryHandle cache for the filesystem. Holds strong references to DirEntry objects. For
    /// filesystems with permanent entries, this will hold a strong reference to every node to make
    /// sure it doesn't get freed without being explicitly unlinked. Otherwise, entries are
    /// maintained in an LRU cache.
    entries: Entries,

    /// Holds security state for this file system, which is created and used by the Linux Security
    /// Modules subsystem hooks.
    pub security_state: security::FileSystemState,
}

#[derive(Clone, Debug, Default)]
pub struct FileSystemOptions {
    /// The source string passed as the first argument to mount(), e.g. a block device.
    pub source: FsString,
    /// Flags kept per-superblock, i.e. included in MountFlags::STORED_ON_FILESYSTEM.
    pub flags: MountFlags,
    /// Filesystem options passed as the last argument to mount().
    pub params: MountParams,
}

impl FileSystemOptions {
    pub fn source_for_display(&self) -> &FsStr {
        if self.source.is_empty() {
            return "none".into();
        }
        self.source.as_ref()
    }
}

struct LruCache {
    capacity: usize,
    entries: Mutex<LinkedHashMap<ArcKey<DirEntry>, ()>>,
}

enum Entries {
    Permanent(Mutex<HashSet<ArcKey<DirEntry>>>),
    Lru(LruCache),
    Uncached,
}

/// Configuration for CacheMode::Cached.
pub struct CacheConfig {
    pub capacity: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self { capacity: DEFAULT_LRU_CAPACITY }
    }
}

pub enum CacheMode {
    /// Entries are pemanent, instead of a cache of the backing storage. An example is tmpfs: the
    /// DirEntry tree *is* the backing storage, as opposed to ext4, which uses the DirEntry tree as
    /// a cache and removes unused nodes from it.
    Permanent,
    /// Entries are cached.
    Cached(CacheConfig),
    /// Entries are uncached. This can be appropriate in cases where it is difficult for the
    /// filesystem to keep the cache coherent: e.g. the /proc/<pid>/task directory.
    Uncached,
}

impl FileSystem {
    /// Create a new filesystem.
    pub fn new(
        kernel: &Arc<Kernel>,
        cache_mode: CacheMode,
        ops: impl FileSystemOps,
        mut options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let mount_options = security::sb_eat_lsm_opts(&kernel, &mut options.params)?;
        let security_state = security::file_system_init_security(ops.name(), &mount_options)?;

        let file_system = Arc::new(FileSystem {
            kernel: Arc::downgrade(kernel),
            root: OnceLock::new(),
            next_node_id: AtomicU64Counter::new(1),
            ops: Box::new(ops),
            options,
            dev_id: kernel.device_registry.next_anonymous_dev_id(),
            rename_mutex: Mutex::new(()),
            nodes: Mutex::new(HashMap::new()),
            entries: match cache_mode {
                CacheMode::Permanent => Entries::Permanent(Mutex::new(HashSet::new())),
                CacheMode::Cached(CacheConfig { capacity }) => {
                    Entries::Lru(LruCache { capacity, entries: Mutex::new(LinkedHashMap::new()) })
                }
                CacheMode::Uncached => Entries::Uncached,
            },
            security_state,
        });

        // TODO: https://fxbug.dev/366405587 - Workaround to allow SELinux to note that this
        // `FileSystem` needs labeling, once a policy has been loaded.
        security::file_system_post_init_security(kernel, &file_system);

        Ok(file_system)
    }

    #[track_caller]
    pub fn kernel(&self) -> Arc<Kernel> {
        self.kernel.upgrade().expect("cleanup code requiring Kernel must run in Kernel::shutdown")
    }

    pub fn set_root(self: &FileSystemHandle, root: impl FsNodeOps) {
        self.set_root_node(FsNode::new_root(root));
    }

    /// Set up the root of the filesystem. Must not be called more than once.
    pub fn set_root_node(self: &FileSystemHandle, root: FsNode) {
        let root = self.insert_node(root);
        assert!(self.root.set(root).is_ok(), "FileSystem::set_root can't be called more than once");
    }

    /// Inserts a node in the FsNode cache.
    pub fn insert_node(self: &FileSystemHandle, mut node: FsNode) -> DirEntryHandle {
        if node.node_id == 0 {
            node.set_id(self.next_node_id());
        }
        node.set_fs(self);
        let handle: FsNodeHandle = node.into_handle();
        self.nodes.lock().insert(handle.node_id, Arc::downgrade(&handle));
        DirEntry::new(handle, None, FsString::default())
    }

    pub fn has_permanent_entries(&self) -> bool {
        matches!(self.entries, Entries::Permanent(_))
    }

    /// The root directory entry of this file system.
    ///
    /// Panics if this file system does not have a root directory.
    pub fn root(&self) -> &DirEntryHandle {
        self.root.get().unwrap_or_else(|| panic!("FileSystem {} has no root", self.name()))
    }

    /// The root directory entry of this `FileSystem`, if it has one.
    pub fn maybe_root(&self) -> Option<&DirEntryHandle> {
        self.root.get()
    }

    /// Prepare a node for insertion in the node cache.
    ///
    /// Currently, apply the required selinux context if the selinux workaround is enabled on this
    /// filesystem.
    fn prepare_node_for_insertion(
        &self,
        _current_task: &CurrentTask,
        node: &FsNodeHandle,
    ) -> WeakFsNodeHandle {
        Arc::downgrade(node)
    }

    pub fn get_or_create_node<F>(
        &self,
        current_task: &CurrentTask,
        node_id: Option<ino_t>,
        create_fn: F,
    ) -> Result<FsNodeHandle, Errno>
    where
        F: FnOnce(ino_t) -> Result<FsNodeHandle, Errno>,
    {
        self.get_and_validate_or_create_node(current_task, node_id, |_| true, create_fn)
    }

    /// Get a node that is validated with the callback, or create an FsNode for
    /// this file system.
    ///
    /// If node_id is Some, then this function checks the node cache to
    /// determine whether this node is already open. If so, the function
    /// returns the existing FsNode if it passes the validation check. If no
    /// node exists, or a node does but fails the validation check, the function
    /// calls the given create_fn function to create the FsNode.
    ///
    /// If node_id is None, then this function assigns a new identifier number
    /// and calls the given create_fn function to create the FsNode with the
    /// assigned number.
    ///
    /// Returns Err only if create_fn returns Err.
    pub fn get_and_validate_or_create_node<V, C>(
        &self,
        current_task: &CurrentTask,
        node_id: Option<ino_t>,
        validate_fn: V,
        create_fn: C,
    ) -> Result<FsNodeHandle, Errno>
    where
        V: FnOnce(&FsNodeHandle) -> bool,
        C: FnOnce(ino_t) -> Result<FsNodeHandle, Errno>,
    {
        let node_id = node_id.unwrap_or_else(|| self.next_node_id());
        let mut nodes = self.nodes.lock();
        match nodes.entry(node_id) {
            Entry::Vacant(entry) => {
                let node = create_fn(node_id)?;
                entry.insert(self.prepare_node_for_insertion(current_task, &node));
                Ok(node)
            }
            Entry::Occupied(mut entry) => {
                if let Some(node) = entry.get().upgrade() {
                    if validate_fn(&node) {
                        return Ok(node);
                    }
                }
                let node = create_fn(node_id)?;
                entry.insert(self.prepare_node_for_insertion(current_task, &node));
                Ok(node)
            }
        }
    }

    /// File systems that produce their own IDs for nodes should invoke this
    /// function. The ones who leave to this object to assign the IDs should
    /// call |create_node|.
    pub fn create_node_with_id(
        self: &Arc<Self>,
        current_task: &CurrentTask,
        ops: impl Into<Box<dyn FsNodeOps>>,
        id: ino_t,
        info: FsNodeInfo,
    ) -> FsNodeHandle {
        let ops = ops.into();
        let node = FsNode::new_uncached(current_task, ops, self, id, info);
        self.nodes
            .lock()
            .insert(node.node_id, self.prepare_node_for_insertion(current_task, &node));
        node
    }

    pub fn create_node(
        self: &Arc<Self>,
        current_task: &CurrentTask,
        ops: impl Into<Box<dyn FsNodeOps>>,
        info: impl FnOnce(ino_t) -> FsNodeInfo,
    ) -> FsNodeHandle {
        let ops = ops.into();
        let node_id = self.next_node_id();
        self.create_node_with_id(current_task, ops, node_id, info(node_id))
    }

    /// Remove the given FsNode from the node cache.
    ///
    /// Called from the Release trait of FsNode.
    pub fn remove_node(&self, node: &FsNode) {
        let mut nodes = self.nodes.lock();
        if let Some(weak_node) = nodes.get(&node.node_id) {
            if weak_node.strong_count() == 0 {
                nodes.remove(&node.node_id);
            }
        }
    }

    pub fn next_node_id(&self) -> ino_t {
        assert!(!self.ops.generate_node_ids());
        self.next_node_id.next()
    }

    /// Move |renamed| that is at |old_name| in |old_parent| to |new_name| in |new_parent|
    /// replacing |replaced|.
    /// If |replaced| exists and is a directory, this function must check that |renamed| is n
    /// directory and that |replaced| is empty.
    pub fn rename<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        old_parent: &FsNodeHandle,
        old_name: &FsStr,
        new_parent: &FsNodeHandle,
        new_name: &FsStr,
        renamed: &FsNodeHandle,
        replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut locked = locked.cast_locked::<FileOpsCore>();
        self.ops.rename(
            &mut locked,
            self,
            current_task,
            old_parent,
            old_name,
            new_parent,
            new_name,
            renamed,
            replaced,
        )
    }

    /// Exchanges `node1` and `node2`. Parent directory node and the corresponding names
    /// for the two exchanged nodes are passed as `parent1`, `name1`, `parent2`, `name2`.
    pub fn exchange(
        &self,
        current_task: &CurrentTask,
        node1: &FsNodeHandle,
        parent1: &FsNodeHandle,
        name1: &FsStr,
        node2: &FsNodeHandle,
        parent2: &FsNodeHandle,
        name2: &FsStr,
    ) -> Result<(), Errno> {
        self.ops.exchange(self, current_task, node1, parent1, name1, node2, parent2, name2)
    }

    /// Forces a FileSystem unmount.
    // TODO(https://fxbug.dev/394694891): kernel shutdown should ideally unmount FileSystems via
    // their drop impl, which should be triggered by Mount.unmount().
    pub fn force_unmount_ops(&self) {
        self.ops.unmount();
    }

    /// Returns the `statfs` for this filesystem.
    ///
    /// Each `FileSystemOps` impl is expected to override this to return the specific statfs for
    /// the filesystem.
    ///
    /// Returns `ENOSYS` if the `FileSystemOps` don't implement `stat`.
    pub fn statfs<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<statfs, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        security::sb_statfs(current_task, &self)?;
        let mut locked = locked.cast_locked::<FileOpsCore>();
        let mut stat = self.ops.statfs(&mut locked, self, current_task)?;
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
            Entries::Lru(LruCache { entries, .. }) => {
                entries.lock().insert(ArcKey(entry.clone()), ());
            }
            Entries::Uncached => {}
        }
    }

    pub fn will_destroy_dir_entry(&self, entry: &DirEntryHandle) {
        match &self.entries {
            Entries::Permanent(p) => {
                p.lock().remove(ArcKey::ref_cast(entry));
            }
            Entries::Lru(LruCache { entries, .. }) => {
                entries.lock().remove(ArcKey::ref_cast(entry));
            }
            Entries::Uncached => {}
        };
    }

    /// Informs the cache that the entry was used.
    pub fn did_access_dir_entry(&self, entry: &DirEntryHandle) {
        if let Entries::Lru(LruCache { entries, .. }) = &self.entries {
            entries.lock().get_refresh(ArcKey::ref_cast(entry));
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
                let mut entries = l.entries.lock();
                while entries.len() > l.capacity {
                    purged.push(entries.pop_front().unwrap().0 .0);
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
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno>;

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
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
        _old_parent: &FsNodeHandle,
        _old_name: &FsStr,
        _new_parent: &FsNodeHandle,
        _new_name: &FsStr,
        _renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        error!(EROFS)
    }

    fn exchange(
        &self,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
        _node1: &FsNodeHandle,
        _parent1: &FsNodeHandle,
        _name1: &FsStr,
        _node2: &FsNodeHandle,
        _parent2: &FsNodeHandle,
        _name2: &FsStr,
    ) -> Result<(), Errno> {
        error!(EINVAL)
    }

    /// Called when the filesystem is unmounted.
    fn unmount(&self) {}
}

impl Drop for FileSystem {
    fn drop(&mut self) {
        self.ops.unmount();
    }
}

pub type FileSystemHandle = Arc<FileSystem>;
