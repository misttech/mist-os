// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{auth::FsCred, fs::*, lock::*, logging::*, task::CurrentTask, types::*};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

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
            self.items.push(DirEntryInfo { name: name.to_vec(), inode_num, entry_type });
        }
        self.offset = offset;
        Ok(())
    }

    fn offset(&self) -> off_t {
        self.offset
    }
}

struct OverlayNode {
    fs: Arc<OverlayFs>,

    // Corresponding `DirEntries` in the lower and the upper filesystems. At least one must be
    // set. Note that we don't care about `NamespaceNode`: overlayfs overlays filesystems
    // (i.e. not namespace subtrees). These directories may not be mounted anywhere.
    upper: Option<DirEntryHandle>,
    lower: Option<DirEntryHandle>,
}

impl OverlayNode {
    fn new(
        fs: Arc<OverlayFs>,
        upper: Option<DirEntryHandle>,
        lower: Option<DirEntryHandle>,
    ) -> Arc<Self> {
        Arc::new(OverlayNode { fs, upper, lower })
    }

    fn main_entry<'a>(&'a self) -> &'a DirEntryHandle {
        self.upper.as_ref().or(self.lower.as_ref()).expect("Expected either upper or lower node")
    }
}

impl FsNodeOps for Arc<OverlayNode> {
    fn create_file_ops(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        if flags.can_write() {
            not_implemented!("Write support is not implemented yet in overlayfs");
            return error!(EROFS);
        }

        let entry = &self.main_entry();
        let ops: Box<dyn FileOps> = if entry.node.is_dir() {
            Box::new(OverlayDirectory { node: self.clone(), dir_entries: Default::default() })
        } else {
            Box::new(OverlayFile { file: entry.open_anonymous(current_task, flags)? })
        };

        Ok(ops)
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let resolve_child = |dir_opt: &Option<DirEntryHandle>| {
            // TODO(sergeyu): lookup() checks access, but we don't need that here.
            dir_opt
                .as_ref()
                .map(|dir| match dir.component_lookup(current_task, name) {
                    Ok(entry) => Some(Ok(entry)),
                    Err(e) if e.code == ENOENT => None,
                    Err(e) => Some(Err(e)),
                })
                .flatten()
                .transpose()
        };

        let upper: Option<DirEntryHandle> = resolve_child(&self.upper)?;

        let upper_is_dir_opt = upper.as_ref().map(|d| d.node.is_dir());
        let upper_is_dir = upper_is_dir_opt == Some(true);
        let upper_is_file = upper_is_dir_opt == Some(false);

        let lower: Option<DirEntryHandle> = if upper_is_file {
            // We don't need to resolve the lower node if we have a file in the upper dir.
            None
        } else {
            match resolve_child(&self.lower)? {
                // If the upper node is a directory and the lower isn't then ignore the lower node.
                Some(lower) if upper_is_dir && !lower.node.is_dir() => None,
                result => result,
            }
        };

        let info =
            upper.as_ref().or(lower.as_ref()).ok_or_else(|| errno!(ENOENT))?.node.info().clone();

        if is_removed_file(&info) {
            return error!(ENOENT);
        }

        Ok(FsNode::new_uncached(
            Box::new(OverlayNode::new(self.fs.clone(), lower, upper)),
            &node.fs(),
            info.ino,
            info,
        ))
    }

    fn mknod(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _mode: FileMode,
        _dev: DeviceType,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        not_implemented!("Write support is not implemented yet in overlayfs");
        error!(ENOTDIR)
    }

    /// Create and return the given child node as a subdirectory.
    fn mkdir(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _mode: FileMode,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        not_implemented!("Write support is not implemented yet in overlayfs");
        error!(ENOTDIR)
    }

    fn create_symlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _target: &FsStr,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(ENOTDIR)
    }

    fn readlink(&self, node: &FsNode, current_task: &CurrentTask) -> Result<SymlinkTarget, Errno> {
        self.main_entry().node.ops().readlink(node, current_task)
    }

    fn link(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        not_implemented!("Write support is not implemented yet in overlayfs");
        error!(EPERM)
    }

    fn unlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        not_implemented!("Write support is not implemented yet in overlayfs");
        error!(ENOTDIR)
    }

    fn refresh_info<'a>(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        let mut lock = info.write();
        *lock = self.main_entry().node.refresh_info(current_task)?.clone();
        Ok(RwLockWriteGuard::downgrade(lock))
    }
}

struct OverlayDirectory {
    node: Arc<OverlayNode>,
    dir_entries: RwLock<DirEntries>,
}

impl OverlayDirectory {
    fn refresh_dir_entries(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        let mut entries = DirEntries::new();

        let get_directory_items = |dir: &DirEntryHandle| {
            let mut sink = DirentSinkAdapter::default();
            dir.open_anonymous(current_task, OpenFlags::DIRECTORY)?
                .readdir(current_task, &mut sink)?;
            Ok(sink.items)
        };

        let is_removed_entry = |dir: &DirEntryHandle, info: &DirEntryInfo| {
            // We need to lookup the node if the file is a char device.
            if info.entry_type != DirectoryEntryType::CHR {
                return Ok(false);
            }
            let entry = dir.component_lookup(current_task, &info.name)?;
            let result = is_removed_file(&*entry.node.info());
            Ok(result)
        };

        // First enumerate entries in the upper dir. Then enumerate the lower dir and add only
        // items that are not present in the upper.
        let mut upper_set = BTreeSet::new();
        let have_lower = self.node.lower.is_some();
        if let Some(dir) = &self.node.upper {
            for item in get_directory_items(dir)?.drain(..) {
                // Fill `upper_set` only if we will need it later.
                if have_lower {
                    upper_set.insert(item.name.clone());
                }
                if !is_removed_entry(dir, &item)? {
                    entries.push(item);
                }
            }
        }

        if let Some(dir) = &self.node.lower {
            for item in get_directory_items(dir)?.drain(..) {
                if !upper_set.contains(&item.name) && !is_removed_entry(dir, &item)? {
                    entries.push(item);
                }
            }
        }

        *self.dir_entries.write() = entries;

        Ok(())
    }
}

impl FileOps for OverlayDirectory {
    fileops_impl_directory!();

    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        default_seek(current_offset, target, |_| error!(EINVAL))
    }

    fn readdir(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        if sink.offset() == 0 {
            self.refresh_dir_entries(current_task)?;
        }

        emit_dotdot(file, sink)?;

        for item in self.dir_entries.read().iter().skip(sink.offset() as usize - 2) {
            sink.add(item.inode_num, sink.offset() + 1, item.entry_type, &item.name)?;
        }

        Ok(())
    }
}

struct OverlayFile {
    file: FileHandle,
}

impl FileOps for OverlayFile {
    fileops_impl_seekable!();

    fn read(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        self.file.read_at(current_task, offset, data)
    }

    fn write(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        self.file.write_at(current_task, offset, data)
    }

    fn get_vmo(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        length: Option<usize>,
        prot: crate::mm::ProtectionFlags,
    ) -> Result<Arc<fuchsia_zircon::Vmo>, Errno> {
        self.file.get_vmo(current_task, length, prot)
    }
}

pub struct OverlayFs {
    // Keep references to the underlying file systems to ensure they outlive `overlayfs` since
    // they may be unmounted before overlayfs.
    #[allow(unused)]
    lower_fs: FileSystemHandle,
    upper_fs: FileSystemHandle,

    #[allow(unused)]
    work_dir: DirEntryHandle,
}

impl OverlayFs {
    pub fn new_fs(
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let mount_options = fs_args::generic_parse_mount_options(&options.params);
        let lower_dir = resolve_dir_param(current_task, &mount_options, b"lowerdir")?;
        let upper_dir = resolve_dir_param(current_task, &mount_options, b"upperdir")?;
        let work_dir = resolve_dir_param(current_task, &mount_options, b"workdir")?;

        let lower_fs = lower_dir.node.fs().clone();
        let upper_fs = upper_dir.node.fs().clone();

        if !Arc::ptr_eq(&upper_fs, &work_dir.node.fs()) {
            log_error!("overlayfs: upperdir and workdir must be on the same FS");
            return error!(EINVAL);
        }

        let overlay_fs = Arc::new(OverlayFs { lower_fs, upper_fs, work_dir });
        let root_node = OverlayNode::new(overlay_fs.clone(), Some(upper_dir), Some(lower_dir));
        let fs = FileSystem::new(current_task.kernel(), CacheMode::Uncached, overlay_fs, options);
        fs.set_root(root_node);
        Ok(fs)
    }
}

impl FileSystemOps for Arc<OverlayFs> {
    fn statfs(&self, _fs: &FileSystem, current_task: &CurrentTask) -> Result<statfs, Errno> {
        self.upper_fs.statfs(current_task)
    }

    fn name(&self) -> &'static FsStr {
        b"overlay"
    }

    fn rename(
        &self,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
        _old_parent: &FsNodeHandle,
        _old_name: &FsStr,
        _new_parent: &FsNodeHandle,
        _new_name: &FsStr,
        _renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        // TODO(sergeyu): Implement write support.
        error!(EROFS)
    }

    fn unmount(&self) {}
}

/// Helper used to resolve directories passed in mount options. The directory is resolved in the
/// namespace of the calling process, but only `DirEntry` is returned (detached from the
/// namespace). The corresponding file systems may be unmounted before overlayfs that uses them.
fn resolve_dir_param(
    current_task: &CurrentTask,
    mount_options: &HashMap<&FsStr, &FsStr>,
    name: &FsStr,
) -> Result<DirEntryHandle, Errno> {
    let path = mount_options.get(name).ok_or_else(|| {
        log_error!("overlayfs: {} was not specified", String::from_utf8_lossy(name));
        errno!(EINVAL)
    })?;

    current_task
        .open_file(path, OpenFlags::RDONLY | OpenFlags::DIRECTORY)
        .map(|f| f.name.entry.clone())
        .map_err(|e| {
            log_error!("overlayfs: Failed to lookup {}: {}", String::from_utf8_lossy(path), e);
            e
        })
}

/// Returns `true` if the node should be treated as a removed file placeholder. Overlayfs creates
/// a char device with `rder=0` on the upper FS in order to denote deleted files.
fn is_removed_file(info: &FsNodeInfo) -> bool {
    info.mode.is_chr() && info.rdev == DeviceType::NONE
}
