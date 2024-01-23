// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::{CurrentTask, Task},
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_nonseekable, fs_node_impl_dir_readonly, fs_node_impl_not_dir,
        parse_unsigned_file, BytesFile, BytesFileOps, CacheMode, DirectoryEntryType, FileObject,
        FileOps, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode,
        FsNodeHandle, FsNodeOps, FsStr, FsString, StaticDirectoryBuilder, VecDirectory,
        VecDirectoryEntry, VmoFileNode,
    },
};

use selinux::security_server::SecurityServer;
use selinux_common::security_context::SecurityContext;
use selinux_policy::SUPPORTED_POLICY_VERSION;
use starnix_logging::{log_error, log_info, not_implemented};
use starnix_sync::{FileOpsRead, FileOpsWrite, Locked, Mutex};
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

const SELINUX_PERMS: &[&str] = &["add", "find", "read", "set"];

struct SeLinuxFs;
impl FileSystemOps for SeLinuxFs {
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        Ok(statfs::default(SELINUX_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        "selinuxfs".into()
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
            Some(security_server) => security_server,
            None => {
                return error!(EINVAL);
            }
        };

        // Read-only files & directories, exposing SELinux internal state.
        dir.entry(current_task, "checkreqprot", SeCheckReqProt::new_node(), mode!(IFREG, 0o644));
        dir.entry(current_task, "class", SeLinuxClassDirectory::new(), mode!(IFDIR, 0o777));
        dir.entry(
            current_task,
            "deny_unknown",
            SeDenyUnknown::new_node(security_server.clone()),
            mode!(IFREG, 0o444),
        );
        dir.entry(
            current_task,
            "reject_unknown",
            SeRejectUnknown::new_node(security_server.clone()),
            mode!(IFREG, 0o444),
        );
        dir.subdir(current_task, "initial_contexts", 0o555, |dir| {
            dir.entry(
                current_task,
                "kernel",
                BytesFile::new_node(b"system_u:system_r:kernel_t:s0".to_vec()),
                mode!(IFREG, 0o444),
            );
        });
        dir.entry(current_task, "mls", BytesFile::new_node(b"1".to_vec()), mode!(IFREG, 0o444));
        dir.entry(
            current_task,
            "policy",
            SePolicy::new_node(security_server.clone()),
            mode!(IFREG, 0o600),
        );
        dir.entry(
            current_task,
            "policyvers",
            BytesFile::new_node(format!("{}", SUPPORTED_POLICY_VERSION).as_bytes().to_vec()),
            mode!(IFREG, 0o444),
        );
        dir.entry(
            current_task,
            "status",
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
        dir.entry(current_task, "access", AccessFileNode::new(), mode!(IFREG, 0o666));
        dir.entry(
            current_task,
            "context",
            SeContext::new_node(security_server.clone()),
            mode!(IFREG, 0o666),
        );
        dir.entry(current_task, "create", SeCreate::new_node(), mode!(IFREG, 0o666));
        dir.entry(
            current_task,
            "load",
            SeLoad::new_node(security_server.clone()),
            mode!(IFREG, 0o600),
        );

        // Allows the SELinux enforcing mode to be queried, or changed.
        dir.entry(
            current_task,
            "enforce",
            SeEnforce::new_node(security_server.clone()),
            // TODO(b/297313229): Get mode from the container.
            mode!(IFREG, 0o644),
        );

        // "/dev/null" equivalent used for file descriptors redirected by SELinux.
        dir.entry_dev(current_task, "null", DeviceFileNode, mode!(IFCHR, 0o666), DeviceType::NULL);

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
        log_info!("Loading {} byte policy", data.len());
        self.security_server.load_policy(data).map_err(|error| {
            log_error!("Policy load error: {}", error);
            errno!(EINVAL)
        })
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
    security_server: Arc<SecurityServer>,
}

impl SeEnforce {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        BytesFile::new_node(Self { security_server })
    }
}

impl BytesFileOps for SeEnforce {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let enforce_data = parse_unsigned_file::<u32>(&data)?;
        self.security_server.set_enforcing(match enforce_data {
            0 => false,
            1 => true,
            _ => error!(EINVAL)?,
        });
        Ok(())
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(format!("{}", self.security_server.enforcing() as u32).as_bytes().to_vec().into())
    }
}

struct SeDenyUnknown {
    security_server: Arc<SecurityServer>,
}

impl SeDenyUnknown {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        BytesFile::new_node(Self { security_server })
    }
}

impl BytesFileOps for SeDenyUnknown {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(format!("{}", self.security_server.deny_unknown() as u32).as_bytes().to_vec().into())
    }
}

struct SeRejectUnknown {
    security_server: Arc<SecurityServer>,
}

impl SeRejectUnknown {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        BytesFile::new_node(Self { security_server })
    }
}

impl BytesFileOps for SeRejectUnknown {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(format!("{}", self.security_server.reject_unknown() as u32).as_bytes().to_vec().into())
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

struct SeContext {
    security_server: Arc<SecurityServer>,
}

impl SeContext {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        BytesFile::new_node(Self { security_server })
    }
}

impl BytesFileOps for SeContext {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        // Validate that `data` holds a valid UTF-8 encoded Security Context
        // string.
        let security_context = SecurityContext::try_from(data).map_err(|_| errno!(EINVAL))?;

        // Validate that the `SecurityContext` refers to principals, types, etc
        // that actually exist in the current policy, by attempting to create
        // a SID from it.
        let _sid = self.security_server.security_context_to_sid(&security_context);

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
        _locked: &mut Locked<'_, FileOpsRead>,
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
        _locked: &mut Locked<'_, FileOpsWrite>,
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
            .entry(name.to_owned())
            .or_insert_with(|| {
                let index = format!("{next_index}\n").into_bytes();
                let fs = node.fs();
                let mut dir = StaticDirectoryBuilder::new(&fs);
                dir.entry(current_task, "index", BytesFile::new_node(index), mode!(IFREG, 0o444));
                dir.subdir(current_task, "perms", 0o555, |perms| {
                    for (i, perm) in SELINUX_PERMS.iter().enumerate() {
                        let node = BytesFile::new_node(format!("{}\n", i + 1).into_bytes());
                        perms.entry(current_task, perm, node, mode!(IFREG, 0o444));
                    }
                });
                dir.set_mode(mode!(IFDIR, 0o555));
                dir.build(current_task)
            })
            .clone())
    }
}

pub fn selinux_proc_attrs(
    current_task: &CurrentTask,
    task: &TempRef<'_, Task>,
    dir: &mut StaticDirectoryBuilder<'_>,
) {
    use SeProcAttrNodeType::*;
    dir.entry(current_task, "current", Current.new_node(task), mode!(IFREG, 0o666));
    dir.entry(current_task, "exec", Exec.new_node(task), mode!(IFREG, 0o666));
    dir.entry(current_task, "fscreate", FsCreate.new_node(task), mode!(IFREG, 0o666));
    dir.entry(current_task, "keycreate", KeyCreate.new_node(task), mode!(IFREG, 0o666));
    dir.entry(current_task, "prev", Previous.new_node(task), mode!(IFREG, 0o666));
    dir.entry(current_task, "sockcreate", SockCreate.new_node(task), mode!(IFREG, 0o666));
}

enum SeProcAttrNodeType {
    Current,
    Exec,
    FsCreate,
    KeyCreate,
    Previous,
    SockCreate,
}

struct SeProcAttrNode {
    attr: SeProcAttrNodeType,
    task: WeakRef<Task>,
}

impl SeProcAttrNodeType {
    fn new_node(self, task: &TempRef<'_, Task>) -> impl FsNodeOps {
        BytesFile::new_node(SeProcAttrNode { attr: self, task: WeakRef::from(task) })
    }
}

impl BytesFileOps for SeProcAttrNode {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let task = Task::from_weak(&self.task)?;

        // If SELinux is disabled then no writes are accepted.
        let security_server = task.kernel().security_server.as_ref().ok_or(errno!(EINVAL))?;

        // Attempt to convert the Security Context string to a SID.
        // Writes that consist of a single NUL or a newline clear the SID.
        let sid = match data.as_slice() {
            b"\x0a" | b"\x00" => None,
            _ => {
                let security_context =
                    SecurityContext::try_from(data).map_err(|_| errno!(EINVAL))?;
                Some(security_server.security_context_to_sid(&security_context))
            }
        };

        // SELinux is enabled, so the task must have `selinux_state`. Lock it for writing
        // and update it.
        let mut tg = task.thread_group.write();
        let selinux_state = tg.selinux_state.as_mut().unwrap();

        use SeProcAttrNodeType::*;
        match self.attr {
            Current => selinux_state.current_sid = sid.ok_or(errno!(EINVAL))?,
            Exec => selinux_state.exec_sid = sid,
            FsCreate => selinux_state.fscreate_sid = sid,
            KeyCreate => selinux_state.keycreate_sid = sid,
            Previous => {
                return error!(EINVAL);
            }
            SockCreate => selinux_state.sockcreate_sid = sid,
        };

        Ok(())
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        use SeProcAttrNodeType::*;
        let task = Task::from_weak(&self.task)?;

        // If SELinux is disabled then all reads are rejected, except for "current".
        match &task.kernel().security_server {
            Some(security_server) => {
                // Read the specified SELinux attribute's SID.
                let sid = {
                    let tg = task.thread_group.read();
                    tg.selinux_state.as_ref().and_then(|selinux_state| match self.attr {
                        Current => Some(selinux_state.current_sid),
                        Exec => selinux_state.exec_sid,
                        FsCreate => selinux_state.fscreate_sid,
                        KeyCreate => selinux_state.keycreate_sid,
                        Previous => Some(selinux_state.previous_sid),
                        SockCreate => selinux_state.sockcreate_sid,
                    })
                };

                // Convert it to a Security Context string.
                let security_context =
                    sid.and_then(|sid| security_server.sid_to_security_context(&sid));

                Ok(security_context
                    .map_or(Vec::new(), |context| format!("{}", context).as_bytes().to_vec())
                    .into())
            }
            None => match self.attr {
                Current => Ok(b"unconfined".to_vec().into()),
                _ => error!(EINVAL),
            },
        }
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
