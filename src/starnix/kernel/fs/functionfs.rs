// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::CurrentTask,
    vfs::{
        fileops_impl_nonseekable, fs_args, fs_node_impl_dir_readonly, fs_node_impl_not_dir,
        CacheMode, DirectoryEntryType, FileObject, FileOps, FileSystem, FileSystemHandle,
        FileSystemOps, FileSystemOptions, FsNode, FsNodeInfo, FsNodeOps, FsStr, InputBuffer,
        OutputBuffer, VecDirectory, VecDirectoryEntry,
    },
};
use bstr::B;
use fidl_fuchsia_hardware_adb as fadb;
use fuchsia_zircon as zx;
use starnix_logging::{log_warn, track_stub};
use starnix_sync::{FileOpsCore, Locked, Mutex, WriteOps};
use starnix_uapi::{
    errno, error,
    errors::Errno,
    file_mode::mode,
    gid_t, ino_t,
    open_flags::OpenFlags,
    statfs, uid_t, usb_functionfs_event, usb_functionfs_event_type_FUNCTIONFS_BIND,
    usb_functionfs_event_type_FUNCTIONFS_ENABLE,
    vfs::{default_statfs, FdEvents},
};
use std::{collections::VecDeque, sync::Arc};
use zerocopy::IntoBytes;

// The node identifiers of different nodes in FunctionFS.
const ROOT_NODE_ID: ino_t = 1;

// Control endpoint is always present in a mounted FunctionFS.
const CONTROL_ENDPOINT: &str = "ep0";
const CONTROL_ENDPOINT_NODE_ID: ino_t = 2;

const OUTPUT_ENDPOINT: &str = "ep1";
const OUTPUT_ENDPOINT_NODE_ID: ino_t = 3;

const INPUT_ENDPOINT: &str = "ep2";
const INPUT_ENDPOINT_NODE_ID: ino_t = 4;

// Magic number of the file system, different from the magic used for Descriptors and Strings.
// Set to the same value as Linux.
const FUNCTIONFS_MAGIC: u32 = 0xa647361;

const ADB_DIRECTORY: &str = "/dev/class/adb";

type AdbHandle = Arc<fadb::UsbAdbImpl_SynchronousProxy>;

pub struct FunctionFs;
impl FunctionFs {
    pub fn new_fs(
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        if options.source != "adb" {
            track_stub!(TODO("https://fxbug.dev/329699340"), "FunctionFS supports other uses");
            return error!(ENODEV);
        }

        // ADB daemon assumes that ADB works over USB if FunctionFS is able to mount.
        // Check that the ADB directory capability is provided to the kernel, and fail to mount
        // if it is not.
        if let Err(e) = std::fs::read_dir(ADB_DIRECTORY) {
            log_warn!(
                "Attempted to mount FunctionFS for adb, but could not read {ADB_DIRECTORY}: {e}"
            );
            return error!(ENODEV);
        }

        let fs = FileSystem::new(current_task.kernel(), CacheMode::Uncached, FunctionFs, options);

        let mut mount_options = fs_args::generic_parse_mount_options(fs.options.params.as_ref())?;
        let uid = if let Some(uid) = mount_options.remove(B("uid")) {
            fs_args::parse::<uid_t>(uid.as_ref())?
        } else {
            0
        };
        let gid = if let Some(gid) = mount_options.remove(B("gid")) {
            fs_args::parse::<gid_t>(gid.as_ref())?
        } else {
            0
        };

        let mut root = FsNode::new_root_with_properties(FunctionFsRootDir::default(), |info| {
            info.ino = ROOT_NODE_ID;
            info.uid = uid;
            info.gid = gid;
        });
        root.node_id = ROOT_NODE_ID;
        fs.set_root_node(root);

        Ok(fs)
    }
}

impl FileSystemOps for FunctionFs {
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        Ok(default_statfs(FUNCTIONFS_MAGIC))
    }

    fn name(&self) -> &'static FsStr {
        b"functionfs".into()
    }
}

#[derive(Default)]
struct FunctionFsState {
    // Keeps track of the number of FileObject's created for the control endpoint.
    // When all FileObjects are closed, the filesystem resets to its initial state.
    // See https://docs.kernel.org/usb/functionfs.html.
    num_control_file_objects: usize,

    // Whether the FunctionFS has input/output endpoints, which are /ep2 and /ep1
    // respectively. /ep0 is the control endpoint and is always available.
    has_input_output_endpoints: bool,

    // FIDL bindings to the Adb driver, for reading and writing to a device.
    adb_proxy: Option<AdbHandle>,

    // FIDL binding to the adb driver, for start and stop calls.
    device_proxy: Option<fadb::DeviceSynchronousProxy>,

    // FunctionFs events that indicate the connection state, to be read through
    // the control endpoint.
    event_queue: VecDeque<usb_functionfs_event>,

    // Whether FunctionFS has received the initial ADB header from the host.
    // Used to reject unexpected payloads when an ADB header has not been read.
    has_received_initial_adb_header: bool,
}

fn connect_to_device() -> Result<(fadb::DeviceSynchronousProxy, AdbHandle), Errno> {
    let mut dir = std::fs::read_dir(ADB_DIRECTORY).map_err(|_| errno!(EINVAL))?;

    if let Some(Ok(entry)) = dir.next() {
        let path = entry.path().into_os_string().into_string().map_err(|_| errno!(EINVAL))?;

        let (client_channel, server_channel) = zx::Channel::create();
        fdio::service_connect(&path, server_channel).map_err(|_| errno!(EINVAL))?;
        let device_proxy = fadb::DeviceSynchronousProxy::new(client_channel);

        let (adb_proxy, server_end) =
            fidl::endpoints::create_sync_proxy::<fadb::UsbAdbImpl_Marker>();
        device_proxy
            .start(server_end, zx::Time::INFINITE)
            .map_err(|_| errno!(EINVAL))?
            .map_err(|_| errno!(EINVAL))?;

        return Ok((device_proxy, Arc::new(adb_proxy)));
    }
    error!(EBUSY)
}

#[derive(Default)]
struct FunctionFsRootDir {
    state: Mutex<FunctionFsState>,
}

impl FunctionFsRootDir {
    fn create_endpoints(&self) -> Result<(), Errno> {
        let mut state = self.state.lock();

        // create_endpoints can be called multiple times as descriptors are written
        // to the control endpoint.
        if state.has_input_output_endpoints {
            return Ok(());
        }
        let result = connect_to_device()?;
        state.device_proxy = Some(result.0);
        state.adb_proxy = Some(result.1);
        state.has_input_output_endpoints = true;

        // Currently FunctionFS assumes the device is always online.
        track_stub!(TODO("https://fxbug.dev/329699340"), "FunctionFS correctly handles USB events");

        state.event_queue.push_back(usb_functionfs_event {
            type_: usb_functionfs_event_type_FUNCTIONFS_BIND as u8,
            ..Default::default()
        });
        state.event_queue.push_back(usb_functionfs_event {
            type_: usb_functionfs_event_type_FUNCTIONFS_ENABLE as u8,
            ..Default::default()
        });
        Ok(())
    }

    fn on_control_opened(&self) {
        let mut state = self.state.lock();
        state.num_control_file_objects += 1;
    }

    fn on_control_closed(&self) {
        let mut state = self.state.lock();
        state.num_control_file_objects -= 1;
        if state.num_control_file_objects == 0 {
            // When all control endpoints are closed, the filesystem resets to its initial state.
            state.has_input_output_endpoints = false;
            state.adb_proxy = None;
            state.event_queue.clear();
            state.has_received_initial_adb_header = false;

            let _ = state
                .device_proxy
                .as_ref()
                .expect("Device Proxy is required")
                .stop(zx::Time::INFINITE)
                .map_err(|_| errno!(EINVAL));
        }
    }

    fn available(&self) -> usize {
        let state = self.state.lock();
        state.event_queue.len()
    }

    fn get_adb(&self) -> Result<AdbHandle, Errno> {
        let state = self.state.lock();
        if let Some(adb_proxy) = state.adb_proxy.as_ref() {
            return Ok(adb_proxy.clone());
        }
        error!(ENODEV)
    }

    // Hack for ADB: Discard ADB messages until the first ADB header is read.
    // Returns false if an unexpected payload is received before the first ADB header.
    //
    // ADB daemon expects a 24-byte header message followed by 0 or more payload messages, and restarts
    // if it reads an unexpected message. This resets FunctionFS, and causes ADB host to resend a connect
    // request. The header of this request is likely to be missed again by FunctionFS as it comes back up,
    // resulting in ADB daemon crash looping. See https://fxbug.dev/330386381 for details.
    //
    // This hack assumes that reads are not performed concurrently. While this is true for ADB daemon,
    // where reads arrive from a read queue, there's nothing that guarantees this for other users.
    fn should_forward_adb_message_to_daemon(&self, bytes: &[u8]) -> bool {
        const ADB_HEADER_LEN: usize = 24;
        let mut state = self.state.lock();

        if state.has_received_initial_adb_header {
            return true;
        }

        if bytes.len() == ADB_HEADER_LEN {
            state.has_received_initial_adb_header = true;
            true
        } else {
            log_warn!("Expected ADB header, instead got message of size {}", bytes.len());
            false
        }
    }
}

impl FsNodeOps for FunctionFsRootDir {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let mut entries = vec![];
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: CONTROL_ENDPOINT.into(),
            inode: Some(CONTROL_ENDPOINT_NODE_ID),
        });

        let state = self.state.lock();
        if state.has_input_output_endpoints {
            entries.push(VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: INPUT_ENDPOINT.into(),
                inode: Some(INPUT_ENDPOINT_NODE_ID),
            });
            entries.push(VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: OUTPUT_ENDPOINT.into(),
                inode: Some(OUTPUT_ENDPOINT_NODE_ID),
            });
        }

        Ok(VecDirectory::new_file(entries))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<crate::vfs::FsNodeHandle, Errno> {
        let name = std::str::from_utf8(name).map_err(|_| errno!(ENOENT))?;
        match name {
            CONTROL_ENDPOINT => Ok(node.fs().create_node_with_id(
                current_task,
                FunctionFsControlEndpoint,
                CONTROL_ENDPOINT_NODE_ID,
                FsNodeInfo::new(CONTROL_ENDPOINT_NODE_ID, mode!(IFREG, 0o600), node.info().cred()),
            )),
            OUTPUT_ENDPOINT => Ok(node.fs().create_node_with_id(
                current_task,
                FunctionFsOutputEndpoint,
                OUTPUT_ENDPOINT_NODE_ID,
                FsNodeInfo::new(OUTPUT_ENDPOINT_NODE_ID, mode!(IFREG, 0o600), node.info().cred()),
            )),
            INPUT_ENDPOINT => Ok(node.fs().create_node_with_id(
                current_task,
                FunctionFsInputEndpoint,
                INPUT_ENDPOINT_NODE_ID,
                FsNodeInfo::new(INPUT_ENDPOINT_NODE_ID, mode!(IFREG, 0o600), node.info().cred()),
            )),
            _ => error!(ENOENT),
        }
    }
}

// FunctionFS Control Endpoint is both readable and writable.
// Clients should write USB descriptors to the endpoint to setup the USB connection.
// Clients can read `usb_functionfs_event`s to know about the USB connection state.
struct FunctionFsControlEndpoint;
impl FsNodeOps for FunctionFsControlEndpoint {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let fs = node.fs();
        let rootdir = fs
            .root()
            .node
            .downcast_ops::<FunctionFsRootDir>()
            .expect("failed to downcast functionfs root dir");
        rootdir.on_control_opened();
        Ok(Box::new(FunctionFsControlEndpoint))
    }
}

impl FileOps for FunctionFsControlEndpoint {
    fileops_impl_nonseekable!();

    fn close(&self, file: &FileObject, _current_task: &CurrentTask) {
        let rootdir = file
            .fs
            .root()
            .node
            .downcast_ops::<FunctionFsRootDir>()
            .expect("failed to downcast functionfs root dir");
        rootdir.on_control_closed();
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        // The control endpoint does not currently implement blocking read.
        // ADB would only read from this endpoint after polling it.
        track_stub!(
            TODO("https://fxbug.dev/329699340"),
            "FunctionFS blocking read on control endpoint"
        );

        let rootdir = file
            .fs
            .root()
            .node
            .downcast_ops::<FunctionFsRootDir>()
            .expect("failed to downcast functionfs root dir");

        let mut state = rootdir.state.lock();
        if !state.event_queue.is_empty() {
            if data.available() < std::mem::size_of::<usb_functionfs_event>() {
                return error!(EINVAL);
            }
        } else {
            return error!(EAGAIN);
        }
        let front = state.event_queue.pop_front().expect("pop from non-empty event queue");
        data.write(front.as_bytes())
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        // The ADB driver creates and passes its own descriptors to the host system over the wire,
        // and so, Starnix does not need to parse the descriptors that Android sends.
        // Here we directly attempt to connect to the driver via FIDL, and create endpoints for data transfer.
        track_stub!(TODO("https://fxbug.dev/329699340"), "FunctionFS should parse descriptors");

        let rootdir = file
            .fs
            .root()
            .node
            .downcast_ops::<FunctionFsRootDir>()
            .expect("failed to downcast functionfs root dir");
        rootdir.create_endpoints()?;

        Ok(data.drain())
    }

    fn query_events(
        &self,
        file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let rootdir = file
            .fs
            .root()
            .node
            .downcast_ops::<FunctionFsRootDir>()
            .expect("failed to downcast functionfs root dir");
        if rootdir.available() > 0 {
            Ok(FdEvents::POLLIN)
        } else {
            Ok(FdEvents::empty())
        }
    }
}

// FunctionFSInputEndpoint is device to host communication, a.k.a. the "IN" USB direction.
// This endpoint is writable, and not readable.
struct FunctionFsInputEndpoint;
impl FsNodeOps for FunctionFsInputEndpoint {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(FunctionFsInputEndpoint))
    }
}

impl FileOps for FunctionFsInputEndpoint {
    fileops_impl_nonseekable!();

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(EINVAL)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let bytes = data.read_all()?;
        let rootdir = file
            .fs
            .root()
            .node
            .downcast_ops::<FunctionFsRootDir>()
            .expect("failed to downcast functionfs root dir");
        let adb = rootdir.get_adb()?;

        match adb.queue_tx(&bytes, zx::Time::INFINITE) {
            Err(err) => {
                log_warn!("Failed to call UsbAdbImpl.QueueTx: {err}");
                return error!(EINVAL);
            }
            Ok(Err(err)) => {
                log_warn!("Failed to queue data to adb driver: {err}");
                return error!(EINVAL);
            }
            Ok(Ok(_)) => Ok(bytes.len()),
        }
    }
}

// FunctionFSOutputEndpoint is host to device communication, a.k.a. the "OUT" USB direction.
// This endpoint is readable, and not writable.
struct FunctionFsOutputEndpoint;
impl FsNodeOps for FunctionFsOutputEndpoint {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(FunctionFsOutputFileObject))
    }
}

struct FunctionFsOutputFileObject;

impl FileOps for FunctionFsOutputFileObject {
    fileops_impl_nonseekable!();

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let rootdir = file
            .fs
            .root()
            .node
            .downcast_ops::<FunctionFsRootDir>()
            .expect("failed to downcast functionfs root dir");
        let adb = rootdir.get_adb()?;

        match adb.receive(zx::Time::INFINITE) {
            Err(err) => {
                log_warn!("Failed to call UsbAdbImpl.Receive: {err}");
                return error!(EINVAL);
            }
            Ok(Err(err)) => {
                log_warn!("Failed to receive data from adb driver: {err}");
                return error!(EINVAL);
            }
            Ok(Ok(payload)) => {
                if payload.len() > data.available() {
                    // This means the data will only be partially written, with the rest discarded.
                    // Instead of attempting this, we'll instead return error to the client.
                    return error!(EINVAL);
                }

                // Hack for ADB. See comment for `should_forward_adb_message_to_daemon`.
                if rootdir.should_forward_adb_message_to_daemon(&payload) {
                    data.write(&payload)
                } else {
                    log_warn!("Sending a reset message back to ADB host to force a retry.");

                    // Arbitrary non-empty and non-zero message that does not look like a real ADB message.
                    let garbage = [12, 23, 34, 45, 56, 67];
                    match adb.queue_tx(&garbage, zx::Time::INFINITE) {
                        Err(err) => {
                            log_warn!("Failed to call UsbAdbImpl.QueueTx: {err}");
                        }
                        Ok(Err(err)) => {
                            log_warn!("Failed to queue garbage message to adb driver: {err}");
                        }
                        Ok(Ok(_)) => {}
                    }
                    Ok(0)
                }
            }
        }
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EINVAL)
    }
}
