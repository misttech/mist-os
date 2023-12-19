// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::{CurrentTask, Task},
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_nonseekable, fs_node_impl_dir_readonly, fs_node_impl_not_dir,
        parse_unsigned_file, serialize_u32_file, BytesFile, BytesFileOps, CacheMode,
        DirectoryEntryType, FileObject, FileOps, FileSystem, FileSystemHandle, FileSystemOps,
        FileSystemOptions, FsNode, FsNodeHandle, FsNodeOps, FsStr, FsString,
        StaticDirectoryBuilder, VecDirectory, VecDirectoryEntry, VmoFileNode,
    },
};
use derivative::Derivative;
use selinux::security_server::SecurityServer;
use selinux_policy::SUPPORTED_POLICY_VERSION;
use starnix_logging::{log_debug, not_implemented};
use starnix_sync::Mutex;
use starnix_uapi::{
    device_type::DeviceType,
    errno, error,
    errors::Errno,
    file_mode::mode,
    open_flags::OpenFlags,
    ownership::{TempRef, WeakRef},
    statfs, SELINUX_MAGIC,
};
use std::{borrow::Cow, collections::BTreeMap, sync::Arc};
use zerocopy::{AsBytes, NoCell};

/// The version of selinux_status_t this kernel implements.
const SELINUX_STATUS_VERSION: u32 = 1;

const SELINUX_PERMS: &[&[u8]] = &[b"add", b"find", b"read", b"set"];

struct SeLinuxFs;
impl FileSystemOps for SeLinuxFs {
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        Ok(statfs::default(SELINUX_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        b"selinuxfs"
    }
}

/// Implements the /sys/fs/selinux filesystem, as documented in the SELinux
/// Notebook at
/// https://github.com/SELinuxProject/selinux-notebook/blob/main/src/lsm_selinux.md#selinux-filesystem
impl SeLinuxFs {
    fn new_fs(
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let kernel = current_task.kernel();
        let fs = FileSystem::new(kernel, CacheMode::Permanent, SeLinuxFs, options);
        let mut dir = StaticDirectoryBuilder::new(&fs);

        // There should always be a SecurityServer if SeLinuxFs is active.
        let security_server = match kernel.security_server.as_ref() {
            Some(security_server) => security_server.clone(),
            None => {
                return error!(EINVAL);
            }
        };

        // Read-only files & directories, exposing SELinux internal state.
        dir.entry(current_task, b"checkreqprot", SeCheckReqProt::new_node(), mode!(IFREG, 0o644));
        dir.entry(current_task, b"class", SeLinuxClassDirectory::new(), mode!(IFDIR, 0o777));
        dir.entry(
            current_task,
            b"deny_unknown",
            // Allow all unknown object classes/permissions.
            BytesFile::new_node(b"0:0\n".to_vec()),
            mode!(IFREG, 0o444),
        );
        dir.subdir(current_task, b"initial_contexts", 0o555, |dir| {
            dir.entry(
                current_task,
                b"kernel",
                BytesFile::new_node(b"system_u:system_r:kernel_t:s0".to_vec()),
                mode!(IFREG, 0o444),
            );
        });
        dir.entry(current_task, b"mls", BytesFile::new_node(b"1".to_vec()), mode!(IFREG, 0o444));
        dir.entry(
            current_task,
            b"policy",
            SePolicy::new_node(security_server.clone()),
            mode!(IFREG, 0o600),
        );
        dir.entry(
            current_task,
            b"policyvers",
            BytesFile::new_node(serialize_u32_file(SUPPORTED_POLICY_VERSION)),
            mode!(IFREG, 0o444),
        );
        dir.entry(
            current_task,
            b"status",
            // The status file needs to be mmap-able, so use a VMO-backed file.
            // When the selinux state changes in the future, the way to update this data (and
            // communicate updates with userspace) is to use the
            // ["seqlock"](https://en.wikipedia.org/wiki/Seqlock) technique.
            VmoFileNode::from_bytes(
                selinux_status_t { version: SELINUX_STATUS_VERSION, ..Default::default() }
                    .as_bytes(),
            )?,
            mode!(IFREG, 0o444),
        );

        // Write-only files used to configure and query SELinux.
        dir.entry(current_task, b"access", AccessFileNode::new(), mode!(IFREG, 0o666));
        dir.entry(current_task, b"context", BytesFile::new_node(SeContext), mode!(IFREG, 0o666));
        dir.entry(current_task, b"create", SeCreate::new_node(), mode!(IFREG, 0o644));
        dir.entry(
            current_task,
            b"load",
            SeLoad::new_node(security_server.clone()),
            mode!(IFREG, 0o600),
        );

        // Allows the SELinux enforcing mode to be queried, or changed.
        dir.entry(
            current_task,
            b"enforce",
            SeEnforce::new_node(),
            // TODO(b/297313229): Get mode from the container.
            mode!(IFREG, 0o644),
        );

        // "/dev/null" equivalent used for file descriptors redirected by SELinux.
        dir.entry_dev(current_task, b"null", DeviceFileNode, mode!(IFCHR, 0o666), DeviceType::NULL);

        dir.build_root();

        Ok(fs)
    }
}

/// The C-style struct exposed to userspace by the /sys/fs/selinux/status file.
/// Defined here (instead of imported through bindgen) as selinux headers are not exposed through
/// kernel uapi headers.
#[derive(Debug, Copy, Clone, AsBytes, NoCell, Default)]
#[repr(C, packed)]
struct selinux_status_t {
    /// Version number of this structure (1).
    version: u32,
    /// Sequence number. See [seqlock](https://en.wikipedia.org/wiki/Seqlock).
    sequence: u32,
    /// `0` means permissive mode, `1` means enforcing mode.
    enforcing: u32,
    /// The number of times the selinux policy has been reloaded.
    policyload: u32,
    /// `0` means allow and `1` means deny unknown object classes/permissions.
    deny_unknown: u32,
}

struct SeLoad {
    security_server: Arc<SecurityServer>,
}

impl SeLoad {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        BytesFile::new_node(Self { security_server })
    }
}

impl BytesFileOps for SeLoad {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        not_implemented!("ignoring selinux policy");
        self.security_server.load_policy(data).map_err(|_| errno!(EINVAL))
    }
}

struct SePolicy {
    security_server: Arc<SecurityServer>,
}

impl SePolicy {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        BytesFile::new_node(Self { security_server })
    }
}

impl BytesFileOps for SePolicy {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(self.security_server.get_binary_policy().into())
    }
}

struct SeEnforce {
    enforce: Mutex<bool>,
}

impl SeEnforce {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self { enforce: Mutex::new(false) })
    }
}

impl BytesFileOps for SeEnforce {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let enforce_data = parse_unsigned_file::<u32>(&data)?;
        let enforce_opt = match enforce_data {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        if let Some(enforce) = enforce_opt {
            *self.enforce.lock() = enforce;
        }
        Ok(())
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(serialize_u32_file(*self.enforce.lock() as u32).into())
    }
}

struct SeCreate {
    data: Mutex<Vec<u8>>,
}

impl SeCreate {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self { data: Mutex::default() })
    }
}

impl BytesFileOps for SeCreate {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        *self.data.lock() = data;
        Ok(())
    }
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(self.data.lock().clone().into())
    }
}

struct SeCheckReqProt;

impl SeCheckReqProt {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self {})
    }
}

impl BytesFileOps for SeCheckReqProt {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let _checkreqprot = parse_unsigned_file::<u32>(&data)?;
        not_implemented!("selinux checkreqprot");
        Ok(())
    }
}

struct SeContext;
impl BytesFileOps for SeContext {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        not_implemented!("selinux validate context");
        log_debug!("selinux context: {}", String::from_utf8_lossy(&data));
        Ok(())
    }
}

struct AccessFile {
    seqno: u64,
}

impl FileOps for AccessFile {
    fileops_impl_nonseekable!();

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        // Format is allowed decided autitallow auditdeny seqno flags
        // Everything but seqno must be in hexadecimal format and represents a bits field.
        let content = format!("ffffffff ffffffff 0 ffffffff {} 0\n", self.seqno);
        data.write(content.as_bytes())
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        Ok(data.drain())
    }
}

struct DeviceFileNode;
impl FsNodeOps for DeviceFileNode {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        unreachable!("Special nodes cannot be opened.");
    }
}

struct AccessFileNode {
    seqno: Mutex<u64>,
}

impl AccessFileNode {
    fn new() -> Self {
        Self { seqno: Mutex::new(0) }
    }
}

impl FsNodeOps for AccessFileNode {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let seqno = {
            let mut writer = self.seqno.lock();
            *writer += 1;
            *writer
        };
        Ok(Box::new(AccessFile { seqno }))
    }
}

struct SeLinuxClassDirectory {
    entries: Mutex<BTreeMap<FsString, FsNodeHandle>>,
}

impl SeLinuxClassDirectory {
    fn new() -> Arc<Self> {
        Arc::new(Self { entries: Mutex::new(BTreeMap::new()) })
    }
}

impl FsNodeOps for Arc<SeLinuxClassDirectory> {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(
            self.entries
                .lock()
                .iter()
                .map(|(name, node)| VecDirectoryEntry {
                    entry_type: DirectoryEntryType::DIR,
                    name: name.clone(),
                    inode: Some(node.node_id),
                })
                .collect(),
        ))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let mut entries = self.entries.lock();
        let next_index = entries.len() + 1;
        Ok(entries
            .entry(name.to_vec())
            .or_insert_with(|| {
                let index = format!("{next_index}\n").into_bytes();
                let fs = node.fs();
                let mut dir = StaticDirectoryBuilder::new(&fs);
                dir.entry(current_task, b"index", BytesFile::new_node(index), mode!(IFREG, 0o444));
                dir.subdir(current_task, b"perms", 0o555, |perms| {
                    for (i, perm) in SELINUX_PERMS.iter().enumerate() {
                        let node = BytesFile::new_node(format!("{}\n", i + 1).as_bytes().to_vec());
                        perms.entry(current_task, perm, node, mode!(IFREG, 0o444));
                    }
                });
                dir.set_mode(mode!(IFDIR, 0o555));
                dir.build(current_task)
            })
            .clone())
    }
}

#[derive(Derivative)]
#[derivative(Default)]
pub struct SeLinuxThreadGroupState {
    #[derivative(Default(
        value = "b\"unconfined_u:unconfined_r:unconfined_t:s0-s0:c0-c1023\".to_vec()"
    ))]
    pub current_context: FsString,
    pub fscreate_context: FsString,
    pub exec_context: FsString,
}

pub fn selinux_proc_attrs(
    current_task: &CurrentTask,
    task: &TempRef<'_, Task>,
    dir: &mut StaticDirectoryBuilder<'_>,
) {
    use SeLinuxContextAttr::*;
    dir.entry(current_task, b"current", Current.new_node(task), mode!(IFREG, 0o666));
    dir.entry(current_task, b"fscreate", FsCreate.new_node(task), mode!(IFREG, 0o666));
    dir.entry(current_task, b"exec", Exec.new_node(task), mode!(IFREG, 0o666));
}

enum SeLinuxContextAttr {
    Current,
    Exec,
    FsCreate,
}

impl SeLinuxContextAttr {
    fn new_node(self, task: &TempRef<'_, Task>) -> impl FsNodeOps {
        BytesFile::new_node(AttrNode { attr: self, task: WeakRef::from(task) })
    }

    fn access_on_task<R, F: FnOnce(&mut FsString) -> R>(&self, task: &Task, f: F) -> R {
        let mut tg = task.thread_group.write();
        match self {
            Self::Current => f(&mut tg.selinux.current_context),
            Self::FsCreate => f(&mut tg.selinux.fscreate_context),
            Self::Exec => f(&mut tg.selinux.exec_context),
        }
    }
}

struct AttrNode {
    attr: SeLinuxContextAttr,
    task: WeakRef<Task>,
}

impl BytesFileOps for AttrNode {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        self.attr.access_on_task(Task::from_weak(&self.task)?.as_ref(), |attr| *attr = data);
        Ok(())
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(self
            .attr
            .access_on_task(Task::from_weak(&self.task)?.as_ref(), |attr| attr.clone())
            .into())
    }
}

/// # Panics
///
/// Will panic if the supplied `kern` is not configured with SELinux enabled.
pub fn selinux_fs(current_task: &CurrentTask, options: FileSystemOptions) -> &FileSystemHandle {
    current_task.kernel().selinux_fs.get_or_init(|| {
        SeLinuxFs::new_fs(current_task, options).expect("failed to construct selinuxfs")
    })
}
