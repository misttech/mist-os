// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::*;
use crate::lock::{Mutex, RwLock};
use crate::logging::not_implemented;
use crate::task::*;
use crate::types::*;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;
use zerocopy::AsBytes;

/// The version of selinux_status_t this kernel implements.
const SELINUX_STATUS_VERSION: u32 = 1;

const SELINUX_PERMS: &[&[u8]] = &[b"add", b"find", b"set"];

struct SeLinuxFs;
impl FileSystemOps for SeLinuxFs {
    fn statfs(&self, _fs: &FileSystem) -> Result<statfs, Errno> {
        Ok(statfs::default(SELINUX_MAGIC))
    }
}

impl SeLinuxFs {
    fn new_fs(kernel: &Kernel) -> Result<FileSystemHandle, Errno> {
        let fs = FileSystem::new_with_permanent_entries(kernel, SeLinuxFs);
        StaticDirectoryBuilder::new(&fs)
            .entry(b"load", BytesFile::new_node(SeLoad), mode!(IFREG, 0o600))
            .entry(b"enforce", BytesFile::new_node(SeEnforce), mode!(IFREG, 0o644))
            .entry(b"checkreqprot", BytesFile::new_node(SeCheckReqProt), mode!(IFREG, 0o644))
            .entry(b"access", AccessFileNode::new(), mode!(IFREG, 0o666))
            .entry(
                b"deny_unknown",
                // Allow all unknown object classes/permissions.
                BytesFile::new_node(b"0:0\n".to_vec()),
                mode!(IFREG, 0o444),
            )
            .entry(
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
            )
            .entry(b"class", SeLinuxClassDirectory::new(), mode!(IFDIR, 0o777))
            .entry(b"context", BytesFile::new_node(SeContext), mode!(IFREG, 0o644))
            .entry_dev(b"null", DeviceFileNode {}, mode!(IFCHR, 0o666), DeviceType::NULL)
            .build_root();

        Ok(fs)
    }
}

/// The C-style struct exposed to userspace by the /sys/fs/selinux/status file.
/// Defined here (instead of imported through bindgen) as selinux headers are not exposed through
/// kernel uapi headers.
#[derive(Debug, Copy, Clone, AsBytes, Default)]
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

struct SeLoad;
impl BytesFileOps for SeLoad {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        not_implemented!(current_task, "got selinux policy, length {}, ignoring", data.len());
        Ok(())
    }
}

struct SeEnforce;
impl BytesFileOps for SeEnforce {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let enforce = parse_int(&data)?;
        not_implemented!(current_task, "selinux setenforce: {}", enforce);
        Ok(())
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(b"0\n"[..].into())
    }
}

struct SeCheckReqProt;
impl BytesFileOps for SeCheckReqProt {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let checkreqprot = parse_int(&data)?;
        not_implemented!(current_task, "selinux checkreqprot: {}", checkreqprot);
        Ok(())
    }
}

struct SeContext;
impl BytesFileOps for SeContext {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        not_implemented!(
            current_task,
            "selinux validate context: {}",
            String::from_utf8_lossy(&data)
        );
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
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        // Format is allowed decided autitallow auditdeny seqno flags
        // Everything but seqno must be in hexadecimal format and represents a bits field.
        let content = format!("ffffffff ffffffff 0 ffffffff {} 0\n", self.seqno);
        data.write(content.as_bytes())
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        Ok(data.drain())
    }
}

struct DeviceFileNode;

impl FsNodeOps for DeviceFileNode {
    fn create_file_ops(
        &self,
        _node: &FsNode,
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
    fn create_file_ops(
        &self,
        _node: &FsNode,
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
    entries: RwLock<BTreeMap<FsString, FsNodeHandle>>,
}

impl SeLinuxClassDirectory {
    fn new() -> Arc<Self> {
        Arc::new(Self { entries: RwLock::new(BTreeMap::new()) })
    }
}

impl FsNodeOps for Arc<SeLinuxClassDirectory> {
    fn create_file_ops(
        &self,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(
            self.entries
                .read()
                .iter()
                .map(|(name, node)| VecDirectoryEntry {
                    entry_type: DirectoryEntryType::DIR,
                    name: name.clone(),
                    inode: Some(node.inode_num),
                })
                .collect(),
        ))
    }

    fn lookup(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let mut entries = self.entries.write();
        let next_index = entries.len() + 1;
        Ok(entries
            .entry(name.to_vec())
            .or_insert_with(|| {
                let index = format!("{next_index}\n").into_bytes();
                StaticDirectoryBuilder::new(&node.fs())
                    .entry(b"index", BytesFile::new_node(index), mode!(IFREG, 0o444))
                    .subdir(b"perms", 0o555, |mut perms| {
                        for (i, perm) in SELINUX_PERMS.iter().enumerate() {
                            let node =
                                BytesFile::new_node(format!("{}\n", i + 1).as_bytes().to_vec());
                            perms = perms.entry(perm, node, mode!(IFREG, 0o444));
                        }
                        perms
                    })
                    .set_mode(mode!(IFDIR, 0o555))
                    .build()
            })
            .clone())
    }
}

fn parse_int(buf: &[u8]) -> Result<u32, Errno> {
    let i = buf.iter().position(|c| !char::from(*c).is_ascii_digit()).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..i]).unwrap().parse::<u32>().map_err(|_| errno!(EINVAL))
}

pub fn selinux_fs(kern: &Kernel) -> &FileSystemHandle {
    kern.selinux_fs.get_or_init(|| SeLinuxFs::new_fs(kern).expect("failed to construct selinuxfs"))
}
