// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "512"]

mod seq_lock;

use seq_lock::SeqLock;

use starnix_core::mm::memory::MemoryObject;
use starnix_core::security;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::buffers::{InputBuffer, OutputBuffer};
use starnix_core::vfs::{
    emit_dotdot, fileops_impl_directory, fileops_impl_noop_sync, fileops_impl_seekable,
    fs_node_impl_dir_readonly, fs_node_impl_not_dir, parse_unsigned_file, unbounded_seek,
    BytesFile, BytesFileOps, CacheMode, DirectoryEntryType, DirentSink, FileObject, FileOps,
    FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode, FsNodeHandle,
    FsNodeInfo, FsNodeOps, FsStr, FsString, MemoryFileNode, SeekTarget, SimpleFileNode,
    StaticDirectoryBuilder, VecDirectory, VecDirectoryEntry,
};

use fuchsia_zircon::{self as zx, HandleBased as _};
use selinux::policy::SUPPORTED_POLICY_VERSION;
use selinux::{
    InitialSid, SeLinuxStatus, SeLinuxStatusPublisher, SecurityId, SecurityPermission,
    SecurityServer,
};
use starnix_logging::{impossible_error, log_error, log_info, track_stub};
use starnix_sync::{FileOpsCore, Locked, Unlocked};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::vfs::default_statfs;
use starnix_uapi::{errno, error, off_t, statfs, SELINUX_MAGIC};
use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use zerocopy::{AsBytes, NoCell};

/// The version of the SELinux "status" file this implementation implements.
const SELINUX_STATUS_VERSION: u32 = 1;

/// Header of the C-style struct exposed via the /sys/fs/selinux/status file,
/// to userspace. Defined here (instead of imported through bindgen) as selinux
/// headers are not exposed through  kernel uapi headers.
#[derive(AsBytes, Copy, Clone, NoCell)]
#[repr(C, align(4))]
struct SeLinuxStatusHeader {
    /// Version number of this structure (1).
    version: u32,
}

impl Default for SeLinuxStatusHeader {
    fn default() -> Self {
        Self { version: SELINUX_STATUS_VERSION }
    }
}

/// Value part of the C-style struct exposed via the /sys/fs/selinux/status file,
/// to userspace. Defined here (instead of imported through bindgen) as selinux
/// headers are not exposed through  kernel uapi headers.
#[derive(AsBytes, Copy, Clone, Default, NoCell)]
#[repr(C, align(4))]
struct SeLinuxStatusValue {
    /// `0` means permissive mode, `1` means enforcing mode.
    enforcing: u32,
    /// The number of times the selinux policy has been reloaded.
    policyload: u32,
    /// `0` means allow and `1` means deny unknown object classes/permissions.
    deny_unknown: u32,
}

type StatusSeqLock = SeqLock<SeLinuxStatusHeader, SeLinuxStatusValue>;

impl SeLinuxStatusPublisher for StatusSeqLock {
    fn set_status(&mut self, policy_status: SeLinuxStatus) {
        self.set_value(SeLinuxStatusValue {
            enforcing: policy_status.is_enforcing as u32,
            policyload: policy_status.change_count,
            deny_unknown: policy_status.deny_unknown as u32,
        })
    }
}

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
        // If SELinux is not enabled then the "selinuxfs" file system does not exist.
        let security_server = security::selinuxfs_get_admin_api(current_task)
            .ok_or_else(|| errno!(ENODEV, "selinuxfs"))?;

        let kernel = current_task.kernel();
        let fs = FileSystem::new(kernel, CacheMode::Permanent, SeLinuxFs, options)?;
        let mut dir = StaticDirectoryBuilder::new(&fs);

        // Read-only files & directories, exposing SELinux internal state.
        dir.entry(current_task, "checkreqprot", CheckReqProtApi::new_node(), mode!(IFREG, 0o644));
        dir.entry(
            current_task,
            "class",
            ClassDirectory::new(security_server.clone()),
            mode!(IFDIR, 0o555),
        );
        dir.entry(
            current_task,
            "deny_unknown",
            DenyUnknownFile::new_node(security_server.clone()),
            mode!(IFREG, 0o444),
        );
        dir.entry(
            current_task,
            "reject_unknown",
            RejectUnknownFile::new_node(security_server.clone()),
            mode!(IFREG, 0o444),
        );
        dir.subdir(current_task, "initial_contexts", 0o555, |dir| {
            for initial_sid in InitialSid::all_variants().into_iter() {
                dir.entry(
                    current_task,
                    initial_sid.name(),
                    InitialContextFile::new_node(security_server.clone(), initial_sid),
                    mode!(IFREG, 0o444),
                );
            }
        });
        dir.entry(current_task, "mls", BytesFile::new_node(b"1".to_vec()), mode!(IFREG, 0o444));
        dir.entry(
            current_task,
            "policy",
            PolicyFile::new_node(security_server.clone()),
            mode!(IFREG, 0o600),
        );
        dir.entry(
            current_task,
            "policyvers",
            BytesFile::new_node(format!("{}", SUPPORTED_POLICY_VERSION).into_bytes()),
            mode!(IFREG, 0o444),
        );

        // The status file needs to be mmap-able, so use a VMO-backed file. When the selinux state
        // changes in the future, the way to update this data (and communicate updates with
        // userspace) is to use the ["seqlock"](https://en.wikipedia.org/wiki/Seqlock) technique.
        let status_holder = StatusSeqLock::new_default().expect("selinuxfs status seqlock");
        let status_file = status_holder
            .get_readonly_vmo()
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .map_err(impossible_error)?;
        dir.entry(
            current_task,
            "status",
            MemoryFileNode::from_memory(Arc::new(MemoryObject::from(status_file))),
            mode!(IFREG, 0o444),
        );
        security_server.set_status_publisher(Box::new(status_holder));

        // Write-only files used to configure and query SELinux.
        dir.entry(current_task, "access", AccessApi::new_node(), mode!(IFREG, 0o666));
        dir.entry(
            current_task,
            "context",
            ContextApi::new_node(security_server.clone()),
            mode!(IFREG, 0o666),
        );
        dir.entry(current_task, "create", CreateApi::new_node(), mode!(IFREG, 0o666));
        dir.entry(
            current_task,
            "load",
            LoadApi::new_node(security_server.clone()),
            mode!(IFREG, 0o600),
        );
        dir.entry(
            current_task,
            "commit_pending_bools",
            CommitBooleansApi::new_node(security_server.clone()),
            mode!(IFREG, 0o200),
        );

        // Read/write files allowing values to be queried or changed.
        dir.entry(
            current_task,
            "booleans",
            BooleansDirectory::new(security_server.clone()),
            mode!(IFDIR, 0o555),
        );
        dir.entry(
            current_task,
            "enforce",
            EnforceApi::new_node(security_server.clone()),
            // TODO(b/297313229): Get mode from the container.
            mode!(IFREG, 0o644),
        );

        // "/dev/null" equivalent used for file descriptors redirected by SELinux.
        dir.entry_dev(current_task, "null", DeviceFileNode, mode!(IFCHR, 0o666), DeviceType::NULL);

        dir.build_root();

        Ok(fs)
    }
}

/// "load" API, accepting a binary policy in a single `write()` operation, which must be at seek
/// position zero.
struct LoadApi {
    security_server: Arc<SecurityServer>,
}

impl LoadApi {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        SeLinuxApi::new_node(move || Ok(Self { security_server: security_server.clone() }))
    }
}

impl SeLinuxApiOps for LoadApi {
    fn api_write_permission() -> SecurityPermission {
        SecurityPermission::LoadPolicy
    }
    fn api_write(&self, offset: usize, data: Vec<u8>) -> Result<(), Errno> {
        if offset != 0 {
            return error!(EINVAL);
        }
        log_info!("Loading {} byte policy", data.len());
        self.security_server.load_policy(data).map_err(|error| {
            log_error!("Policy load error: {}", error);
            errno!(EINVAL)
        })
    }
}

/// "policy" file, which allows the currently-loaded binary policy, to be read as a normal file,
/// including supporting seek-aware reads.
struct PolicyFile {
    security_server: Arc<SecurityServer>,
}

impl PolicyFile {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        BytesFile::new_node(Self { security_server })
    }
}

impl BytesFileOps for PolicyFile {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(self.security_server.get_binary_policy().into())
    }
}

/// "enforce" API used to control whether SELinux is globally permissive, versus enforcing.
struct EnforceApi {
    security_server: Arc<SecurityServer>,
}

impl EnforceApi {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        SeLinuxApi::new_node(move || Ok(Self { security_server: security_server.clone() }))
    }
}

impl SeLinuxApiOps for EnforceApi {
    fn api_write_permission() -> SecurityPermission {
        SecurityPermission::SetEnforce
    }

    fn api_write(&self, _offset: usize, data: Vec<u8>) -> Result<(), Errno> {
        let enforce = parse_unsigned_file::<u32>(&data)? != 0;
        self.security_server.set_enforcing(enforce);
        Ok(())
    }

    fn api_read(&self, _offset: usize) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(format!("{}", self.security_server.is_enforcing() as u32).into_bytes().into())
    }
}

/// "deny_unknown" file which exposes how classes & permissions not defined by the policy should
/// be allowed or denied.
struct DenyUnknownFile {
    security_server: Arc<SecurityServer>,
}

impl DenyUnknownFile {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        BytesFile::new_node(Self { security_server })
    }
}

impl BytesFileOps for DenyUnknownFile {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(format!("{}", self.security_server.deny_unknown() as u32).into_bytes().into())
    }
}

/// "reject_unknown" file which exposes whether kernel classes & permissions not defined by the
/// policy would have prevented the policy being loaded.
struct RejectUnknownFile {
    security_server: Arc<SecurityServer>,
}

impl RejectUnknownFile {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        BytesFile::new_node(Self { security_server })
    }
}

impl BytesFileOps for RejectUnknownFile {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(format!("{}", self.security_server.reject_unknown() as u32).into_bytes().into())
    }
}

/// "create" API used to determine the Security Context to associate with a new resource instance
/// based on source, target, and target class.
struct CreateApi;

impl CreateApi {
    fn new_node() -> impl FsNodeOps {
        SeLinuxApi::new_node(|| Ok(Self {}))
    }
}

impl SeLinuxApiOps for CreateApi {
    fn api_write_permission() -> SecurityPermission {
        SecurityPermission::ComputeCreate
    }
    fn api_write(&self, _offset: usize, _data: Vec<u8>) -> Result<(), Errno> {
        track_stub!(TODO("https://fxbug.dev/361552580"), "selinux create");
        Ok(())
    }
    fn api_read(&self, _offset: usize) -> Result<Cow<'_, [u8]>, Errno> {
        Ok([].as_ref().into())
    }
}

struct CheckReqProtApi;

impl CheckReqProtApi {
    fn new_node() -> impl FsNodeOps {
        SeLinuxApi::new_node(|| Ok(Self {}))
    }
}

impl SeLinuxApiOps for CheckReqProtApi {
    fn api_write_permission() -> SecurityPermission {
        SecurityPermission::SetCheckReqProt
    }

    fn api_write(&self, _offset: usize, data: Vec<u8>) -> Result<(), Errno> {
        let _checkreqprot = parse_unsigned_file::<u32>(&data)? != 0;
        track_stub!(TODO("https://fxbug.dev/322874766"), "selinux checkreqprot");
        Ok(())
    }
}

/// "context" API which accepts a Security Context in a single `write()` operation, whose result indicates
/// whether the supplied context was valid with respect to the current policy.
struct ContextApi {
    security_server: Arc<SecurityServer>,
}

impl ContextApi {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        SeLinuxApi::new_node(move || Ok(Self { security_server: security_server.clone() }))
    }
}

impl SeLinuxApiOps for ContextApi {
    fn api_write_permission() -> SecurityPermission {
        SecurityPermission::CheckContext
    }

    fn api_write(&self, _offset: usize, data: Vec<u8>) -> Result<(), Errno> {
        // Validate that the `data` describe valid user, role, type, etc by attempting to create
        // a SID from it.
        // TODO: https://fxbug.dev/362476447 - Provide a validate API that does not allocate a SID.
        self.security_server
            .security_context_to_sid(data.as_slice().into())
            .map_err(|_| errno!(EINVAL))?;
        Ok(())
    }
}

struct InitialContextFile {
    security_server: Arc<SecurityServer>,
    initial_sid: InitialSid,
}

impl InitialContextFile {
    fn new_node(security_server: Arc<SecurityServer>, initial_sid: InitialSid) -> impl FsNodeOps {
        BytesFile::new_node(Self { security_server, initial_sid })
    }
}

impl BytesFileOps for InitialContextFile {
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

struct AccessApi {
    seqno: u64,
}

impl AccessApi {
    fn new_node() -> impl FsNodeOps {
        static SEQUENCE_NO: AtomicU64 = AtomicU64::new(0);
        SeLinuxApi::new_node(move || Ok(Self { seqno: SEQUENCE_NO.fetch_add(1, Ordering::SeqCst) }))
    }
}

impl SeLinuxApiOps for AccessApi {
    fn api_write_permission() -> SecurityPermission {
        SecurityPermission::ComputeAv
    }

    fn api_write(&self, _offset: usize, _data: Vec<u8>) -> Result<(), Errno> {
        track_stub!(TODO("https://fxbug.dev/361551536"), "selinux access");
        Ok(())
    }

    fn api_read(&self, _offset: usize) -> Result<Cow<'_, [u8]>, Errno> {
        // Format is allowed decided auditallow auditdeny seqno flags
        // Everything but seqno must be in hexadecimal format and represents a bits field.
        let result = format!("ffffffff ffffffff 0 ffffffff {} 0\n", self.seqno);
        Ok(result.into_bytes().into())
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

#[derive(Clone)]
struct BooleansDirectory {
    security_server: Arc<SecurityServer>,
}

impl BooleansDirectory {
    fn new(security_server: Arc<SecurityServer>) -> Self {
        Self { security_server }
    }
}

impl FsNodeOps for BooleansDirectory {
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
                BooleanFile::new_node(self.security_server.clone(), utf8_name),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), current_task.as_fscred()),
            ))
        } else {
            error!(ENOENT)
        }
    }
}

impl FileOps for BooleansDirectory {
    fileops_impl_directory!();
    fileops_impl_noop_sync!();

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

struct BooleanFile {
    security_server: Arc<SecurityServer>,
    name: String,
}

impl BooleanFile {
    fn new_node(security_server: Arc<SecurityServer>, name: String) -> impl FsNodeOps {
        BytesFile::new_node(BooleanFile { security_server, name })
    }
}

impl BytesFileOps for BooleanFile {
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
        Ok(format!("{} {}", active as u32, pending as u32).into_bytes().into())
    }
}

struct CommitBooleansApi {
    security_server: Arc<SecurityServer>,
}

impl CommitBooleansApi {
    fn new_node(security_server: Arc<SecurityServer>) -> impl FsNodeOps {
        SeLinuxApi::new_node(move || {
            Ok(CommitBooleansApi { security_server: security_server.clone() })
        })
    }
}

impl SeLinuxApiOps for CommitBooleansApi {
    fn api_write_permission() -> SecurityPermission {
        SecurityPermission::SetBool
    }

    fn api_write(&self, _offset: usize, data: Vec<u8>) -> Result<(), Errno> {
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

struct ClassDirectory {
    security_server: Arc<SecurityServer>,
}

impl ClassDirectory {
    fn new(security_server: Arc<SecurityServer>) -> Self {
        Self { security_server }
    }
}

impl FsNodeOps for ClassDirectory {
    fs_node_impl_dir_readonly!();

    /// Returns the set of classes under the "class" directory.
    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(
            self.security_server
                .class_names()
                .map_err(|_| errno!(ENOENT))?
                .iter()
                .map(|class_name| VecDirectoryEntry {
                    entry_type: DirectoryEntryType::DIR,
                    name: class_name.clone().into(),
                    inode: None,
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
        let fs = node.fs();
        let mut dir = StaticDirectoryBuilder::new(&fs);
        dir.set_mode(mode!(IFDIR, 0o555));
        let id =
            self.security_server.class_id_by_name(&name.to_string()).map_err(|_| errno!(EINVAL))?;
        let index_bytes = format!("{}", id).into_bytes();
        dir.entry(current_task, "index", BytesFile::new_node(index_bytes), mode!(IFREG, 0o444));
        dir.entry(
            current_task,
            "perms",
            PermsDirectory::new(self.security_server.clone(), name.to_string()),
            mode!(IFDIR, 0o555),
        );
        Ok(dir.build(current_task).clone())
    }
}

/// Represents the perms/ directory under each class entry of the SeLinuxClassDirectory.
struct PermsDirectory {
    security_server: Arc<SecurityServer>,
    class_name: String,
}

impl PermsDirectory {
    fn new(security_server: Arc<SecurityServer>, class_name: String) -> Self {
        Self { security_server, class_name }
    }
}

impl FsNodeOps for PermsDirectory {
    fs_node_impl_dir_readonly!();

    /// Lists all available permissions for the corresponding class.
    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(
            self.security_server
                .class_permissions_by_name(&self.class_name)
                .map_err(|_| errno!(ENOENT))?
                .iter()
                .map(|(_permission_id, permission_name)| VecDirectoryEntry {
                    entry_type: DirectoryEntryType::DIR,
                    name: permission_name.clone().into(),
                    inode: None,
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
        let found_permission_id = self
            .security_server
            .class_permissions_by_name(&(self.class_name))
            .map_err(|_| errno!(ENOENT))?
            .iter()
            .find(|(_permission_id, permission_name)| permission_name == name)
            .ok_or_else(|| errno!(ENOENT))?
            .0;

        Ok(node.fs().create_node(
            current_task,
            BytesFile::new_node(format!("{}", found_permission_id).into_bytes()),
            FsNodeInfo::new_factory(mode!(IFREG, 0o444), current_task.as_fscred()),
        ))
    }
}

/// File node implementation tailored to the behaviour of the APIs exposed to userspace via the
/// SELinux filesystem.fx
///
/// API files in the SELinux filesystem have persistent seek offsets, which APIs either
/// ignore (e.g. request-response APIs like "access" or "context") or error-out if non-zero
/// (e.g. the policy "load" file).
///
/// Userspace use of SELinux API files follows a simple request/response flow, with the caller
/// `write()`ing a string of request bytes, and then `read()`ing a string of result bytes
/// from the API file.
struct SeLinuxApi<T: SeLinuxApiOps + Sync + Send + 'static> {
    ops: T,
}

impl<T: SeLinuxApiOps + Sync + Send + 'static> SeLinuxApi<T> {
    /// Returns a new `SeLinuxApi` file node that will use `create_ops` to create a new `SeLinuxApiOps`
    /// instance every time a caller opens the file.
    fn new_node<F>(create_ops: F) -> impl FsNodeOps
    where
        F: Fn() -> Result<T, Errno> + Send + Sync + 'static,
    {
        SimpleFileNode::new(move || create_ops().map(|ops| SeLinuxApi { ops }))
    }
}

/// Trait implemented for each SELinux API file (e.g. "create", "load") to define its behaviour.
trait SeLinuxApiOps {
    /// Returns the "security" class permission that is required in order to write to the API file.
    fn api_write_permission() -> SecurityPermission;

    /// Processes a request written to an API file. Although API files are seekable, the offset
    /// is ignored by most API file implementations, or may be required to be zero.
    fn api_write(&self, _offset: usize, _data: Vec<u8>) -> Result<(), Errno> {
        error!(EINVAL)
    }

    /// Reads the result of a request previously written to the API file. For most APIs the offset
    /// is simply ignored, in contrast to the behaviour implemented by `BytesFile`.
    fn api_read(&self, _offset: usize) -> Result<Cow<'_, [u8]>, Errno> {
        error!(EINVAL)
    }
}

impl<T: SeLinuxApiOps + Sync + Send + 'static> FileOps for SeLinuxApi<T> {
    fileops_impl_seekable!();
    fileops_impl_noop_sync!();

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let response = self.ops.api_read(offset)?;
        data.write(&response)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        security::selinuxfs_check_access(
            current_task,
            &file.name.entry.node,
            T::api_write_permission(),
        )?;
        let data = data.read_all()?;
        let data_len = data.len();
        self.ops.api_write(offset, data)?;
        Ok(data_len)
    }
}

/// Returns the "selinuxfs" file system, used by the system userspace to administer SELinux.
pub fn selinux_fs(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    current_task
        .kernel()
        .selinux_fs
        .get_or_try_init(|| SeLinuxFs::new_fs(current_task, options))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    use fuchsia_zircon::{self as zx, AsHandleRef as _};
    use selinux::SecurityServer;
    use zerocopy::{FromBytes, FromZeroes};

    #[fuchsia::test]
    fn status_vmo_has_correct_size_and_rights() {
        // The current version of the "status" file contains five packed
        // u32 values.
        const STATUS_T_SIZE: usize = size_of::<u32>() * 5;

        let status_holder = StatusSeqLock::new_default().unwrap();
        let status_vmo = status_holder.get_readonly_vmo();

        // Verify the content and actual size of the structure are as expected.
        let content_size = status_vmo.get_content_size().unwrap() as usize;
        assert_eq!(content_size, STATUS_T_SIZE);
        let actual_size = status_vmo.get_size().unwrap() as usize;
        assert!(actual_size >= STATUS_T_SIZE);

        // Ensure the returned handle is read-only and non-resizable.
        let rights = status_vmo.basic_info().unwrap().rights;
        assert_eq!((rights & zx::Rights::MAP), zx::Rights::MAP);
        assert_eq!((rights & zx::Rights::READ), zx::Rights::READ);
        assert_eq!((rights & zx::Rights::GET_PROPERTY), zx::Rights::GET_PROPERTY);
        assert_eq!((rights & zx::Rights::WRITE), zx::Rights::NONE);
        assert_eq!((rights & zx::Rights::RESIZE), zx::Rights::NONE);
    }

    #[derive(FromBytes, FromZeroes)]
    #[repr(C, align(4))]
    struct TestSeLinuxStatusT {
        version: u32,
        sequence: u32,
        enforcing: u32,
        policyload: u32,
        deny_unknown: u32,
    }

    fn with_status_t<R>(
        status_vmo: &Arc<zx::Vmo>,
        do_test: impl FnOnce(&TestSeLinuxStatusT) -> R,
    ) -> R {
        let flags = zx::VmarFlags::PERM_READ
            | zx::VmarFlags::ALLOW_FAULTS
            | zx::VmarFlags::REQUIRE_NON_RESIZABLE;
        let map_addr = fuchsia_runtime::vmar_root_self()
            .map(0, status_vmo, 0, size_of::<TestSeLinuxStatusT>(), flags)
            .unwrap();
        let mapped_status = unsafe { &mut *(map_addr as *mut TestSeLinuxStatusT) };
        let result = do_test(mapped_status);
        unsafe {
            fuchsia_runtime::vmar_root_self()
                .unmap(map_addr, size_of::<TestSeLinuxStatusT>())
                .unwrap()
        };
        result
    }

    #[fuchsia::test]
    fn status_file_layout() {
        let security_server = SecurityServer::new();
        let status_holder = StatusSeqLock::new_default().unwrap();
        let status_vmo = status_holder.get_readonly_vmo();
        security_server.set_status_publisher(Box::new(status_holder));
        security_server.set_enforcing(false);
        let mut seq_no: u32 = 0;
        with_status_t(&status_vmo, |status| {
            assert_eq!(status.version, SELINUX_STATUS_VERSION);
            assert_eq!(status.enforcing, 0);
            seq_no = status.sequence;
            assert_eq!(seq_no % 2, 0);
        });
        security_server.set_enforcing(true);
        with_status_t(&status_vmo, |status| {
            assert_eq!(status.version, SELINUX_STATUS_VERSION);
            assert_eq!(status.enforcing, 1);
            assert_ne!(status.sequence, seq_no);
            seq_no = status.sequence;
            assert_eq!(seq_no % 2, 0);
        });
    }

    #[fuchsia::test]
    fn status_accurate_directly_following_set_status_publisher() {
        let security_server = SecurityServer::new();
        let status_holder = StatusSeqLock::new_default().unwrap();
        let status_vmo = status_holder.get_readonly_vmo();

        // Ensure a change in status-visible security server state is made before invoking
        // `set_status_publisher()`.
        assert_eq!(false, security_server.is_enforcing());
        security_server.set_enforcing(true);

        security_server.set_status_publisher(Box::new(status_holder));
        with_status_t(&status_vmo, |status| {
            // Ensure latest `enforcing` state is reported immediately following
            // `set_status_publisher()`.
            assert_eq!(status.enforcing, 1);
        });
    }
}
