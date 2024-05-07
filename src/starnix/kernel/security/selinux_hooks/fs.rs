// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::CurrentTask,
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        emit_dotdot, fileops_impl_directory, fileops_impl_nonseekable, fs_node_impl_dir_readonly,
        fs_node_impl_not_dir, parse_unsigned_file, unbounded_seek, BytesFile, BytesFileOps,
        CacheMode, DirectoryEntryType, DirentSink, FileObject, FileOps, FileSystem,
        FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode, FsNodeHandle, FsNodeInfo,
        FsNodeOps, FsStr, FsString, SeekTarget, StaticDirectoryBuilder, VecDirectory,
        VecDirectoryEntry, VmoFileNode,
    },
};

use bstr::ByteSlice;
use selinux::{security_server::SecurityServer, InitialSid, SecurityId};
use selinux_policy::SUPPORTED_POLICY_VERSION;
use starnix_logging::{log_error, log_info, track_stub};
use starnix_sync::{FileOpsCore, Locked, Mutex, WriteOps};
use starnix_uapi::{
    device_type::DeviceType, errno, error, errors::Errno, file_mode::mode, off_t,
    open_flags::OpenFlags, statfs, vfs::default_statfs, SELINUX_MAGIC,
};
use std::{borrow::Cow, collections::BTreeMap, sync::Arc};

const SELINUX_PERMS: &[&str] = &["add", "find", "read", "set"];

struct SeLinuxFs;
impl FileSystemOps for SeLinuxFs {
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        Ok(default_statfs(SELINUX_MAGIC))
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
            for initial_sid in InitialSid::all_variants().into_iter() {
                dir.entry(
                    current_task,
                    initial_sid.name(),
                    SeInitialContext::new_node(security_server.clone(), initial_sid),
                    mode!(IFREG, 0o444),
                );
            }
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
            VmoFileNode::from_vmo(security_server.get_status_vmo()),
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
        dir.entry(
            current_task,
            "commit_pending_bools",
            SeLinuxCommitBooleans::new_node(security_server.clone()),
            mode!(IFREG, 0o200),
        );

        // Read/write files allowing values to be queried or changed.
        dir.entry(
            current_task,
            "booleans",
            SeLinuxBooleansDirectory::new(security_server.clone()),
            mode!(IFDIR, 0o555),
        );
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
        track_stub!(TODO("https://fxbug.dev/322874969"), "ignoring selinux policy");
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
        let enforce = parse_unsigned_file::<u32>(&data)? != 0;
        self.security_server.set_enforcing(enforce);
        Ok(())
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(format!("{}", self.security_server.is_enforcing() as u32).as_bytes().to_vec().into())
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
        let _checkreqprot = parse_unsigned_file::<u32>(&data)? != 0;
        track_stub!(TODO("https://fxbug.dev/322874766"), "selinux checkreqprot");
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
        // Validate that the `data` describe valid user, role, type, etc by attempting to create
        // a SID from it.
        let context = data.as_slice().trim_end_with(|c| c == '\0');
        self.security_server.security_context_to_sid(context.into()).map_err(|_| errno!(EINVAL))?;
        Ok(())
    }
}

struct SeInitialContext {
    security_server: Arc<SecurityServer>,
    initial_sid: InitialSid,
}

impl SeInitialContext {
    fn new_node(security_server: Arc<SecurityServer>, initial_sid: InitialSid) -> impl FsNodeOps {
        BytesFile::new_node(Self { security_server, initial_sid })
    }
}

impl BytesFileOps for SeInitialContext {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let sid = SecurityId::initial(self.initial_sid);
        if let Some(context) = self.security_server.sid_to_security_context(sid) {
            Ok(context.into())
        } else {
            // Looking up an initial SID can only fail if no policy is loaded, in
            // which case the file contains the name of the initial SID, rather
            // than a Security Context value.
            Ok(self.initial_sid.name().as_bytes().into())
        }
    }
}

struct AccessFile {
    seqno: u64,
}

impl FileOps for AccessFile {
    fileops_impl_nonseekable!();

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, WriteOps>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
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

struct SeLinuxBooleansDirectory {
    security_server: Arc<SecurityServer>,
}

impl SeLinuxBooleansDirectory {
    fn new(security_server: Arc<SecurityServer>) -> Arc<Self> {
        Arc::new(Self { security_server })
    }
}

impl FsNodeOps for Arc<SeLinuxBooleansDirectory> {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let utf8_name = String::from_utf8(name.to_vec()).map_err(|_| errno!(ENOENT))?;
        if self.security_server.conditional_booleans().contains(&utf8_name) {
            Ok(node.fs().create_node(
                current_task,
                SeLinuxBoolean::new_node(self.security_server.clone(), utf8_name),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), current_task.as_fscred()),
            ))
        } else {
            error!(ENOENT)
        }
    }
}

impl FileOps for SeLinuxBooleansDirectory {
    fileops_impl_directory!();

    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        unbounded_seek(current_offset, target)
    }

    fn readdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        _current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        emit_dotdot(file, sink)?;

        // `emit_dotdot()` provides the first two directory entries, so that the entries for
        // the conditional booleans start from offset 2.
        let iter_offset = sink.offset() - 2;
        for name in self.security_server.conditional_booleans().iter().skip(iter_offset as usize) {
            sink.add(
                file.fs.next_node_id(),
                /* next offset = */ sink.offset() + 1,
                DirectoryEntryType::REG,
                FsString::from(name.as_bytes()).as_ref(),
            )?;
        }

        Ok(())
    }
}

struct SeLinuxBoolean {
    security_server: Arc<SecurityServer>,
    name: String,
}

impl SeLinuxBoolean {
    fn new_node(security_server: Arc<SecurityServer>, name: String) -> impl FsNodeOps {
        BytesFile::new_node(SeLinuxBoolean { security_server, name })
    }
}

impl BytesFileOps for SeLinuxBoolean {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let value = parse_unsigned_file::<u32>(&data)? != 0;
        self.security_server.set_pending_boolean(&self.name, value).map_err(|_| errno!(EIO))
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        // Each boolean has a current active value, and a pending value that
        // will become active if "commit_pending_booleans" is written to.
        // e.g. "1 0" will be read if a boolean is True but will become False.
        let (active, pending) =
            self.security_server.get_boolean(&self.name).map_err(|_| errno!(EIO))?;
        Ok(format!("{} {}", active as u32, pending as u32).as_bytes().to_vec().into())
    }
}

struct SeLinuxCommitBooleans {
    security_server: Arc<SecurityServer>,
}

impl SeLinuxCommitBooleans {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        BytesFile::new_node(SeLinuxCommitBooleans { security_server })
    }
}

impl BytesFileOps for SeLinuxCommitBooleans {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        // "commit_pending_booleans" expects a numeric argument, which is
        // interpreted as a boolean, with the pending booleans committed if the
        // value is true (i.e. non-zero).
        let commit = parse_unsigned_file::<u32>(&data)? != 0;
        if commit {
            self.security_server.commit_pending_booleans();
        }
        Ok(())
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
        _locked: &mut Locked<'_, FileOpsCore>,
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

/// # Panics
///
/// Will panic if the supplied `kern` is not configured with SELinux enabled.
pub fn selinux_fs(current_task: &CurrentTask, options: FileSystemOptions) -> &FileSystemHandle {
    current_task.kernel().selinux_fs.get_or_init(|| {
        SeLinuxFs::new_fs(current_task, options).expect("failed to construct selinuxfs")
    })
}
