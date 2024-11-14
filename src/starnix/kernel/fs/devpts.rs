// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::DeviceMetadata;
use crate::device::terminal::{TTYState, Terminal};
use crate::device::{DeviceMode, DeviceOps};
use crate::fs::devtmpfs::{devtmpfs_create_symlink, devtmpfs_mkdir, devtmpfs_remove_node};
use crate::mm::MemoryAccessorExt;
use crate::task::{CurrentTask, EventHandler, Kernel, WaitCanceler, Waiter};
use crate::vfs::buffers::{InputBuffer, OutputBuffer};
use crate::vfs::{
    fileops_impl_nonseekable, fileops_impl_noop_sync, fs_args, fs_node_impl_dir_readonly,
    CacheMode, DirEntryHandle, DirectoryEntryType, FileHandle, FileObject, FileOps, FileSystem,
    FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode, FsNodeHandle, FsNodeInfo,
    FsNodeOps, FsStr, FsString, SpecialNode, VecDirectory, VecDirectoryEntry,
};
use starnix_logging::{log_error, track_stub};
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, ProcessGroupState, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_types::vfs::default_statfs;
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::{DeviceType, TTY_ALT_MAJOR};
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::signals::SIGWINCH;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{
    errno, error, gid_t, ino_t, pid_t, statfs, uapi, uid_t, DEVPTS_SUPER_MAGIC, FIOASYNC, FIOCLEX,
    FIONBIO, FIONCLEX, FIONREAD, FIOQSIZE, TCFLSH, TCGETA, TCGETS, TCGETX, TCSBRK, TCSBRKP, TCSETA,
    TCSETAF, TCSETAW, TCSETS, TCSETSF, TCSETSW, TCSETX, TCSETXF, TCSETXW, TCXONC, TIOCCBRK,
    TIOCCONS, TIOCEXCL, TIOCGETD, TIOCGICOUNT, TIOCGLCKTRMIOS, TIOCGPGRP, TIOCGPTLCK, TIOCGPTN,
    TIOCGRS485, TIOCGSERIAL, TIOCGSID, TIOCGSOFTCAR, TIOCGWINSZ, TIOCLINUX, TIOCMBIC, TIOCMBIS,
    TIOCMGET, TIOCMIWAIT, TIOCMSET, TIOCNOTTY, TIOCNXCL, TIOCOUTQ, TIOCPKT, TIOCSBRK, TIOCSCTTY,
    TIOCSERCONFIG, TIOCSERGETLSR, TIOCSERGETMULTI, TIOCSERGSTRUCT, TIOCSERGWILD, TIOCSERSETMULTI,
    TIOCSERSWILD, TIOCSETD, TIOCSLCKTRMIOS, TIOCSPGRP, TIOCSPTLCK, TIOCSRS485, TIOCSSERIAL,
    TIOCSSOFTCAR, TIOCSTI, TIOCSWINSZ, TIOCVHANGUP,
};
use std::sync::{Arc, Weak};

use super::sysfs::DeviceDirectory;

// See https://www.kernel.org/doc/Documentation/admin-guide/devices.txt
const DEVPTS_FIRST_MAJOR: u32 = 136;
const DEVPTS_MAJOR_COUNT: u32 = 4;
// The device identifier is encoded through the major and minor device identifier of the
// device. Each major identifier can contain 256 pts replicas.
pub const DEVPTS_COUNT: u32 = DEVPTS_MAJOR_COUNT * 256;
// The block size of the node in the devpts file system. Value has been taken from
// https://github.com/google/gvisor/blob/master/test/syscalls/linux/pty.cc
const BLOCK_SIZE: usize = 1024;

// The node identifier of the different node in the devpts filesystem.
const ROOT_NODE_ID: ino_t = 1;
const PTMX_NODE_ID: ino_t = 2;
const FIRST_PTS_NODE_ID: ino_t = 3;

pub fn dev_pts_fs(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    ensure_devpts(current_task.kernel(), options)
}

fn ensure_devpts(
    kernel: &Arc<Kernel>,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    Ok(kernel
        .dev_pts_fs
        .get_or_init(|| init_devpts(kernel, options).expect("Error when creating default devpts"))
        .clone())
}

/// Creates a terminal and returns the main pty and an associated replica pts.
///
/// This function assumes that `/dev/ptmx` is the `DevPtmxFile` and that devpts
/// is mounted at `/dev/pts`. These assumptions are necessary so that the
/// `FileHandle` objects returned have appropriate `NamespaceNode` objects.
pub fn create_main_and_replica(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    window_size: uapi::winsize,
) -> Result<(FileHandle, FileHandle), Errno> {
    let pty_file = current_task.open_file(locked, "/dev/ptmx".into(), OpenFlags::RDWR)?;
    let pty = pty_file.downcast_file::<DevPtmxFile>().ok_or_else(|| errno!(ENOTTY))?;
    {
        let mut terminal = pty.terminal.write();
        terminal.locked = false;
        terminal.window_size = window_size;
    }
    let pts_path = FsString::from(format!("/dev/pts/{}", pty.terminal.id));
    let pts_file = current_task.open_file(locked, pts_path.as_ref(), OpenFlags::RDWR)?;
    Ok((pty_file, pts_file))
}

fn init_devpts(
    kernel: &Arc<Kernel>,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    let state = kernel.expando.get::<TTYState>();

    let uid =
        options.params.get(b"uid").map(|uid| fs_args::parse::<uid_t>(uid.as_ref())).transpose()?;
    let gid =
        options.params.get(b"gid").map(|gid| fs_args::parse::<gid_t>(gid.as_ref())).transpose()?;

    let fs = FileSystem::new(kernel, CacheMode::Uncached, DevPtsFs { uid, gid }, options)
        .expect("devpts filesystem constructed with valid options");
    let mut root = FsNode::new_root_with_properties(DevPtsRootDir { state }, |info| {
        info.ino = ROOT_NODE_ID;
    });
    root.node_id = ROOT_NODE_ID;
    fs.set_root_node(root);
    Ok(fs)
}

pub fn tty_device_init<L>(locked: &mut Locked<'_, L>, current_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
    let kernel = current_task.kernel();
    let state = kernel.expando.get::<TTYState>();
    let device = DevPtsDevice::new(state);

    let registry = &kernel.device_registry;

    // Register /dev/pts/X device type.
    for n in 0..DEVPTS_MAJOR_COUNT {
        registry
            .register_major("pts".into(), DeviceMode::Char, DEVPTS_FIRST_MAJOR + n, device.clone())
            .expect("can register pts{n} device");
    }

    // Register tty and ptmx device types.
    kernel
        .device_registry
        .register_major("/dev/tty".into(), DeviceMode::Char, TTY_ALT_MAJOR, device)
        .expect("can register tty device");

    let tty_class = registry.objects.tty_class();
    registry.add_device(
        locked,
        current_task,
        "tty".into(),
        DeviceMetadata::new("tty".into(), DeviceType::TTY, DeviceMode::Char),
        tty_class.clone(),
        DeviceDirectory::new,
    );
    registry.add_device(
        locked,
        current_task,
        "ptmx".into(),
        DeviceMetadata::new("ptmx".into(), DeviceType::PTMX, DeviceMode::Char),
        tty_class,
        DeviceDirectory::new,
    );

    devtmpfs_mkdir(locked, current_task, "pts".into()).unwrap();

    // Create a symlink from /dev/ptmx to /dev/pts/ptmx for pseudo-tty subsystem.
    if let Err(err) = devtmpfs_remove_node(locked, current_task, "ptmx".into()) {
        log_error!("Cannot remove device: ptmx ({:?})", err);
    }
    devtmpfs_create_symlink(locked, current_task, "ptmx".into(), "pts/ptmx".into()).unwrap();
}

struct DevPtsFs {
    uid: Option<uid_t>,
    gid: Option<gid_t>,
}

impl FileSystemOps for DevPtsFs {
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(DEVPTS_SUPER_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        "devpts".into()
    }

    fn generate_node_ids(&self) -> bool {
        false
    }
}

// Construct the DeviceType associated with the given pts replicas.
pub fn get_device_type_for_pts(id: u32) -> DeviceType {
    DeviceType::new(DEVPTS_FIRST_MAJOR + id / 256, id % 256)
}

struct DevPtsRootDir {
    state: Arc<TTYState>,
}

impl FsNodeOps for DevPtsRootDir {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let mut result = vec![];
        result.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::CHR,
            name: "ptmx".into(),
            inode: Some(PTMX_NODE_ID),
        });
        for (id, terminal) in self.state.terminals.read().iter() {
            if let Some(terminal) = terminal.upgrade() {
                if !terminal.read().is_main_closed() {
                    result.push(VecDirectoryEntry {
                        entry_type: DirectoryEntryType::CHR,
                        name: format!("{id}").into(),
                        inode: Some((*id as ino_t) + FIRST_PTS_NODE_ID),
                    });
                }
            }
        }
        Ok(VecDirectory::new_file(result))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let name = std::str::from_utf8(name).map_err(|_| errno!(ENOENT))?;
        if name == "ptmx" {
            let mut info = FsNodeInfo::new(PTMX_NODE_ID, mode!(IFCHR, 0o666), FsCred::root());
            info.rdev = DeviceType::PTMX;
            info.blksize = BLOCK_SIZE;
            let node = node.fs().create_node_with_id(current_task, SpecialNode, info.ino, info);
            return Ok(node);
        }
        if let Ok(id) = name.parse::<u32>() {
            let terminal = self.state.terminals.read().get(&id).and_then(Weak::upgrade);
            if let Some(terminal) = terminal {
                if !terminal.read().is_main_closed() {
                    let mut info = FsNodeInfo::new(
                        (id as ino_t) + FIRST_PTS_NODE_ID,
                        mode!(IFCHR, 0o620),
                        terminal.fscred.clone(),
                    );
                    info.rdev = get_device_type_for_pts(id);
                    info.blksize = BLOCK_SIZE;
                    let fs = node.fs();
                    let devptsfs = fs
                        .downcast_ops::<DevPtsFs>()
                        .expect("DevPts should only handle `DevPtsFs`s");
                    info.uid = devptsfs.uid.unwrap_or_else(|| current_task.creds().uid);
                    info.gid = devptsfs.gid.unwrap_or_else(|| current_task.creds().gid);
                    let node = fs.create_node_with_id(current_task, SpecialNode, info.ino, info);
                    return Ok(node);
                }
            }
        }
        error!(ENOENT)
    }
}

struct DevPtsDevice {
    state: Arc<TTYState>,
}

impl DevPtsDevice {
    pub fn new(state: Arc<TTYState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

impl DeviceOps for Arc<DevPtsDevice> {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        current_task: &CurrentTask,
        id: DeviceType,
        _node: &FsNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        match id {
            // /dev/ptmx
            DeviceType::PTMX => {
                let terminal = self.state.get_next_terminal(current_task)?;
                let dev_pts_root =
                    ensure_devpts(current_task.kernel(), Default::default())?.root().clone();

                Ok(Box::new(DevPtmxFile::new(dev_pts_root, terminal)))
            }
            // /dev/tty
            DeviceType::TTY => {
                let controlling_terminal = current_task
                    .thread_group
                    .read()
                    .process_group
                    .session
                    .read()
                    .controlling_terminal
                    .clone();
                if let Some(controlling_terminal) = controlling_terminal {
                    if controlling_terminal.is_main {
                        let dev_pts_root =
                            ensure_devpts(current_task.kernel(), Default::default())?
                                .root()
                                .clone();
                        Ok(Box::new(DevPtmxFile::new(dev_pts_root, controlling_terminal.terminal)))
                    } else {
                        Ok(Box::new(DevPtsFile::new(controlling_terminal.terminal)))
                    }
                } else {
                    error!(ENXIO)
                }
            }
            _ if id.major() < DEVPTS_FIRST_MAJOR
                || id.major() >= DEVPTS_FIRST_MAJOR + DEVPTS_MAJOR_COUNT =>
            {
                error!(ENODEV)
            }
            // /dev/pts/??
            _ => {
                let pts_id = (id.major() - DEVPTS_FIRST_MAJOR) * 256 + id.minor();
                let terminal = self
                    .state
                    .terminals
                    .read()
                    .get(&pts_id)
                    .and_then(Weak::upgrade)
                    .ok_or_else(|| errno!(EIO))?;
                if terminal.read().locked {
                    return error!(EIO);
                }
                if !flags.contains(OpenFlags::NOCTTY) {
                    // Opening a replica sets the process' controlling TTY when possible. An error indicates it cannot
                    // be set, and is ignored silently.
                    let _ = current_task.thread_group.set_controlling_terminal(
                        current_task,
                        &terminal,
                        false, /* is_main */
                        false, /* steal */
                        flags.can_read(),
                    );
                }
                Ok(Box::new(DevPtsFile::new(terminal)))
            }
        }
    }
}

struct DevPtmxFile {
    dev_pts_root: DirEntryHandle,
    terminal: Arc<Terminal>,
}

impl DevPtmxFile {
    pub fn new(dev_pts_root: DirEntryHandle, terminal: Arc<Terminal>) -> Self {
        terminal.main_open();
        Self { dev_pts_root, terminal }
    }
}

impl FileOps for DevPtmxFile {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    fn close(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) {
        self.terminal.main_close();
        let id = FsString::from(self.terminal.id.to_string());
        self.dev_pts_root.remove_child(id.as_ref(), &current_task.kernel().mounts);
    }

    fn read(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(
            locked,
            current_task,
            FdEvents::POLLIN | FdEvents::POLLHUP,
            None,
            |locked| self.terminal.main_read(locked, current_task, data),
        )
    }

    fn write(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(
            locked,
            current_task,
            FdEvents::POLLOUT | FdEvents::POLLHUP,
            None,
            |locked| self.terminal.main_write(locked, current_task, data),
        )
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.terminal.main_wait_async(waiter, events, handler))
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(self.terminal.main_query_events())
    }

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);
        match request {
            TIOCGPTN => {
                // Get the therminal id.
                let value: u32 = self.terminal.id;
                current_task.write_object(UserRef::<u32>::new(user_addr), &value)?;
                Ok(SUCCESS)
            }
            TIOCGPTLCK => {
                // Get the lock status.
                let value = i32::from(self.terminal.read().locked);
                current_task.write_object(UserRef::<i32>::new(user_addr), &value)?;
                Ok(SUCCESS)
            }
            TIOCSPTLCK => {
                // Lock/Unlock the terminal.
                let value = current_task.read_object(UserRef::<i32>::new(user_addr))?;
                self.terminal.write().locked = value != 0;
                Ok(SUCCESS)
            }
            _ => shared_ioctl(locked, &self.terminal, true, _file, current_task, request, arg),
        }
    }
}

struct DevPtsFile {
    terminal: Arc<Terminal>,
}

impl DevPtsFile {
    pub fn new(terminal: Arc<Terminal>) -> Self {
        terminal.replica_open();
        Self { terminal }
    }
}

impl FileOps for DevPtsFile {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    fn close(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) {
        self.terminal.replica_close();
    }

    fn read(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(
            locked,
            current_task,
            FdEvents::POLLIN | FdEvents::POLLHUP,
            None,
            |locked| self.terminal.replica_read(locked, current_task, data),
        )
    }

    fn write(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(
            locked,
            current_task,
            FdEvents::POLLOUT | FdEvents::POLLHUP,
            None,
            |locked| self.terminal.replica_write(locked, current_task, data),
        )
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.terminal.replica_wait_async(waiter, events, handler))
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(self.terminal.replica_query_events())
    }

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        shared_ioctl(locked, &self.terminal, false, file, current_task, request, arg)
    }
}

fn into_termios(value: uapi::termio) -> uapi::termios {
    let mut cc = [0; 19];
    cc[0..8].copy_from_slice(&value.c_cc[0..8]);
    uapi::termios {
        c_iflag: value.c_iflag as uapi::tcflag_t,
        c_oflag: value.c_oflag as uapi::tcflag_t,
        c_cflag: value.c_cflag as uapi::tcflag_t,
        c_lflag: value.c_lflag as uapi::tcflag_t,
        c_line: value.c_line as uapi::cc_t,
        c_cc: cc,
    }
}

fn into_termio(value: uapi::termios) -> uapi::termio {
    let mut cc = [0; 8];
    cc.copy_from_slice(&value.c_cc[0..8]);
    uapi::termio {
        c_iflag: value.c_iflag as u16,
        c_oflag: value.c_oflag as u16,
        c_cflag: value.c_cflag as u16,
        c_lflag: value.c_lflag as u16,
        c_line: value.c_line,
        c_cc: cc,
        ..Default::default()
    }
}

/// The ioctl behaviour common to main and replica terminal file descriptors.
fn shared_ioctl<L>(
    locked: &mut Locked<'_, L>,
    terminal: &Arc<Terminal>,
    is_main: bool,
    file: &FileObject,
    current_task: &CurrentTask,
    request: u32,
    arg: SyscallArg,
) -> Result<SyscallResult, Errno>
where
    L: LockBefore<ProcessGroupState>,
{
    let user_addr = UserAddress::from(arg);
    match request {
        FIONREAD => {
            // Get the main terminal available bytes for reading.
            let value = terminal.read().get_available_read_size(is_main) as u32;
            current_task.write_object(UserRef::<u32>::new(user_addr), &value)?;
            Ok(SUCCESS)
        }
        TIOCSCTTY => {
            // Make the given terminal the controlling terminal of the calling process.
            let steal = bool::from(arg);
            current_task.thread_group.set_controlling_terminal(
                current_task,
                terminal,
                is_main,
                steal,
                file.can_read(),
            )?;
            Ok(SUCCESS)
        }
        TIOCNOTTY => {
            // Release the controlling terminal.
            current_task.thread_group.release_controlling_terminal(
                locked,
                current_task,
                terminal,
                is_main,
            )?;
            Ok(SUCCESS)
        }
        TIOCGPGRP => {
            // Get the foreground process group.
            let pgid = current_task.thread_group.get_foreground_process_group(terminal)?;
            current_task.write_object(UserRef::<pid_t>::new(user_addr), &pgid)?;
            Ok(SUCCESS)
        }
        TIOCSPGRP => {
            // Set the foreground process group.
            let pgid = current_task.read_object(UserRef::<pid_t>::new(user_addr))?;
            current_task.thread_group.set_foreground_process_group(
                locked,
                current_task,
                terminal,
                pgid,
            )?;
            Ok(SUCCESS)
        }
        TIOCGWINSZ => {
            // Get the window size
            current_task.write_object(
                UserRef::<uapi::winsize>::new(user_addr),
                &terminal.read().window_size,
            )?;
            Ok(SUCCESS)
        }
        TIOCSWINSZ => {
            // Set the window size
            terminal.write().window_size =
                current_task.read_object(UserRef::<uapi::winsize>::new(user_addr))?;

            // Send a SIGWINCH signal to the foreground process group.
            let foreground_process_group =
                terminal.read().controller.as_ref().and_then(|terminal_controller| {
                    terminal_controller.get_foreground_process_group()
                });
            if let Some(process_group) = foreground_process_group {
                process_group.send_signals(locked, &[SIGWINCH]);
            }
            Ok(SUCCESS)
        }
        TCGETA => {
            let termio = into_termio(*terminal.read().termios());
            current_task.write_object(UserRef::<uapi::termio>::new(user_addr), &termio)?;
            Ok(SUCCESS)
        }
        TCGETS => {
            // N.B. TCGETS on the main terminal actually returns the configuration of the replica
            // end.
            current_task.write_object(
                UserRef::<uapi::termios>::new(user_addr),
                terminal.read().termios(),
            )?;
            Ok(SUCCESS)
        }
        TCSETA => {
            let termio = current_task.read_object(UserRef::<uapi::termio>::new(user_addr))?;
            terminal.set_termios(locked, into_termios(termio));
            Ok(SUCCESS)
        }
        TCSETS => {
            // N.B. TCSETS on the main terminal actually affects the configuration of the replica
            // end.
            let termios = current_task.read_object(UserRef::<uapi::termios>::new(user_addr))?;
            terminal.set_termios(locked, termios);
            Ok(SUCCESS)
        }
        TCSETAF => {
            // This should drain the output queue and discard the pending input first.
            let termio = current_task.read_object(UserRef::<uapi::termio>::new(user_addr))?;
            terminal.set_termios(locked, into_termios(termio));
            Ok(SUCCESS)
        }
        TCSETSF => {
            // This should drain the output queue and discard the pending input first.
            let termios = current_task.read_object(UserRef::<uapi::termios>::new(user_addr))?;
            terminal.set_termios(locked, termios);
            Ok(SUCCESS)
        }
        TCSETAW => {
            track_stub!(TODO("https://fxbug.dev/322873281"), "TCSETAW drain output queue first");
            let termio = current_task.read_object(UserRef::<uapi::termio>::new(user_addr))?;
            terminal.set_termios(locked, into_termios(termio));
            Ok(SUCCESS)
        }
        TCSETSW => {
            track_stub!(TODO("https://fxbug.dev/322873281"), "TCSETSW drain output queue first");
            let termios = current_task.read_object(UserRef::<uapi::termios>::new(user_addr))?;
            terminal.set_termios(locked, termios);
            Ok(SUCCESS)
        }
        TIOCSETD => {
            track_stub!(
                TODO("https://fxbug.dev/322874060"),
                "devpts setting line discipline",
                is_main
            );
            error!(EINVAL)
        }
        TCSBRK => Ok(SUCCESS),
        TCXONC => {
            track_stub!(TODO("https://fxbug.dev/322892912"), "devpts ioctl TCXONC", is_main);
            error!(ENOSYS)
        }
        TCFLSH => {
            track_stub!(TODO("https://fxbug.dev/322893703"), "devpts ioctl TCFLSH", is_main);
            error!(ENOSYS)
        }
        TIOCEXCL => {
            track_stub!(TODO("https://fxbug.dev/322893449"), "devpts ioctl TIOCEXCL", is_main);
            error!(ENOSYS)
        }
        TIOCNXCL => {
            track_stub!(TODO("https://fxbug.dev/322893393"), "devpts ioctl TIOCNXCL", is_main);
            error!(ENOSYS)
        }
        TIOCOUTQ => {
            track_stub!(TODO("https://fxbug.dev/322893723"), "devpts ioctl TIOCOUTQ", is_main);
            error!(ENOSYS)
        }
        TIOCSTI => {
            track_stub!(TODO("https://fxbug.dev/322893780"), "devpts ioctl TIOCSTI", is_main);
            error!(ENOSYS)
        }
        TIOCMGET => {
            track_stub!(TODO("https://fxbug.dev/322893681"), "devpts ioctl TIOCMGET", is_main);
            error!(ENOSYS)
        }
        TIOCMBIS => {
            track_stub!(TODO("https://fxbug.dev/322893709"), "devpts ioctl TIOCMBIS", is_main);
            error!(ENOSYS)
        }
        TIOCMBIC => {
            track_stub!(TODO("https://fxbug.dev/322893610"), "devpts ioctl TIOCMBIC", is_main);
            error!(ENOSYS)
        }
        TIOCMSET => {
            track_stub!(TODO("https://fxbug.dev/322893211"), "devpts ioctl TIOCMSET", is_main);
            error!(ENOSYS)
        }
        TIOCGSOFTCAR => {
            track_stub!(TODO("https://fxbug.dev/322893365"), "devpts ioctl TIOCGSOFTCAR", is_main);
            error!(ENOSYS)
        }
        TIOCSSOFTCAR => {
            track_stub!(TODO("https://fxbug.dev/322894074"), "devpts ioctl TIOCSSOFTCAR", is_main);
            error!(ENOSYS)
        }
        TIOCLINUX => {
            track_stub!(TODO("https://fxbug.dev/322893147"), "devpts ioctl TIOCLINUX", is_main);
            error!(ENOSYS)
        }
        TIOCCONS => {
            track_stub!(TODO("https://fxbug.dev/322893267"), "devpts ioctl TIOCCONS", is_main);
            error!(ENOSYS)
        }
        TIOCGSERIAL => {
            track_stub!(TODO("https://fxbug.dev/322893503"), "devpts ioctl TIOCGSERIAL", is_main);
            error!(ENOSYS)
        }
        TIOCSSERIAL => {
            track_stub!(TODO("https://fxbug.dev/322893663"), "devpts ioctl TIOCSSERIAL", is_main);
            error!(ENOSYS)
        }
        TIOCPKT => {
            track_stub!(TODO("https://fxbug.dev/322893148"), "devpts ioctl TIOCPKT", is_main);
            error!(ENOSYS)
        }
        FIONBIO => {
            track_stub!(TODO("https://fxbug.dev/322893957"), "devpts ioctl FIONBIO", is_main);
            error!(ENOSYS)
        }
        TIOCGETD => {
            track_stub!(TODO("https://fxbug.dev/322893974"), "devpts ioctl TIOCGETD", is_main);
            error!(ENOSYS)
        }
        TCSBRKP => Ok(SUCCESS),
        TIOCSBRK => {
            track_stub!(TODO("https://fxbug.dev/322893936"), "devpts ioctl TIOCSBRK", is_main);
            error!(ENOSYS)
        }
        TIOCCBRK => {
            track_stub!(TODO("https://fxbug.dev/322893213"), "devpts ioctl TIOCCBRK", is_main);
            error!(ENOSYS)
        }
        TIOCGSID => {
            track_stub!(TODO("https://fxbug.dev/322894076"), "devpts ioctl TIOCGSID", is_main);
            error!(ENOSYS)
        }
        TIOCGRS485 => {
            track_stub!(TODO("https://fxbug.dev/322893728"), "devpts ioctl TIOCGRS485", is_main);
            error!(ENOSYS)
        }
        TIOCSRS485 => {
            track_stub!(TODO("https://fxbug.dev/322893783"), "devpts ioctl TIOCSRS485", is_main);
            error!(ENOSYS)
        }
        TCGETX => {
            track_stub!(TODO("https://fxbug.dev/322893327"), "devpts ioctl TCGETX", is_main);
            error!(ENOSYS)
        }
        TCSETX => {
            track_stub!(TODO("https://fxbug.dev/322893741"), "devpts ioctl TCSETX", is_main);
            error!(ENOSYS)
        }
        TCSETXF => {
            track_stub!(TODO("https://fxbug.dev/322893937"), "devpts ioctl TCSETXF", is_main);
            error!(ENOSYS)
        }
        TCSETXW => {
            track_stub!(TODO("https://fxbug.dev/322893899"), "devpts ioctl TCSETXW", is_main);
            error!(ENOSYS)
        }
        TIOCVHANGUP => {
            track_stub!(TODO("https://fxbug.dev/322893742"), "devpts ioctl TIOCVHANGUP", is_main);
            error!(ENOSYS)
        }
        FIONCLEX => {
            track_stub!(TODO("https://fxbug.dev/322893938"), "devpts ioctl FIONCLEX", is_main);
            error!(ENOSYS)
        }
        FIOCLEX => {
            track_stub!(TODO("https://fxbug.dev/322894214"), "devpts ioctl FIOCLEX", is_main);
            error!(ENOSYS)
        }
        FIOASYNC => {
            track_stub!(TODO("https://fxbug.dev/322893269"), "devpts ioctl FIOASYNC", is_main);
            error!(ENOSYS)
        }
        TIOCSERCONFIG => {
            track_stub!(TODO("https://fxbug.dev/322893881"), "devpts ioctl TIOCSERCONFIG", is_main);
            error!(ENOSYS)
        }
        TIOCSERGWILD => {
            track_stub!(TODO("https://fxbug.dev/322893686"), "devpts ioctl TIOCSERGWILD", is_main);
            error!(ENOSYS)
        }
        TIOCSERSWILD => {
            track_stub!(TODO("https://fxbug.dev/322893837"), "devpts ioctl TIOCSERSWILD", is_main);
            error!(ENOSYS)
        }
        TIOCGLCKTRMIOS => {
            track_stub!(
                TODO("https://fxbug.dev/322894114"),
                "devpts ioctl TIOCGLCKTRMIOS",
                is_main
            );
            error!(ENOSYS)
        }
        TIOCSLCKTRMIOS => {
            track_stub!(
                TODO("https://fxbug.dev/322893711"),
                "devpts ioctl TIOCSLCKTRMIOS",
                is_main
            );
            error!(ENOSYS)
        }
        TIOCSERGSTRUCT => {
            track_stub!(
                TODO("https://fxbug.dev/322893828"),
                "devpts ioctl TIOCSERGSTRUCT",
                is_main
            );
            error!(ENOSYS)
        }
        TIOCSERGETLSR => {
            track_stub!(TODO("https://fxbug.dev/322894083"), "devpts ioctl TIOCSERGETLSR", is_main);
            error!(ENOSYS)
        }
        TIOCSERGETMULTI => {
            track_stub!(
                TODO("https://fxbug.dev/322893962"),
                "devpts ioctl TIOCSERGETMULTI",
                is_main
            );
            error!(ENOSYS)
        }
        TIOCSERSETMULTI => {
            track_stub!(
                TODO("https://fxbug.dev/322893273"),
                "devpts ioctl TIOCSERSETMULTI",
                is_main
            );
            error!(ENOSYS)
        }
        TIOCMIWAIT => {
            track_stub!(TODO("https://fxbug.dev/322894005"), "devpts ioctl TIOCMIWAIT", is_main);
            error!(ENOSYS)
        }
        TIOCGICOUNT => {
            track_stub!(TODO("https://fxbug.dev/322893862"), "devpts ioctl TIOCGICOUNT", is_main);
            error!(ENOSYS)
        }
        FIOQSIZE => {
            track_stub!(TODO("https://fxbug.dev/322893770"), "devpts ioctl FIOQSIZE", is_main);
            error!(ENOSYS)
        }
        other => {
            track_stub!(TODO("https://fxbug.dev/322893712"), "devpts unknown ioctl", other);
            error!(ENOTTY)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::devpts::tty_device_init;
    use crate::fs::tmpfs::TmpFs;
    use crate::testing::*;
    use crate::vfs::buffers::{VecInputBuffer, VecOutputBuffer};
    use crate::vfs::{MountInfo, NamespaceNode};
    use starnix_uapi::auth::Credentials;
    use starnix_uapi::file_mode::{AccessCheck, FileMode};
    use starnix_uapi::signals::{SIGCHLD, SIGTTOU};

    fn ioctl<T: zerocopy::IntoBytes + zerocopy::FromBytes + zerocopy::Immutable + Copy>(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        file: &FileHandle,
        command: u32,
        value: &T,
    ) -> Result<T, Errno> {
        let address = map_memory(
            locked,
            current_task,
            UserAddress::default(),
            std::mem::size_of::<T>() as u64,
        );
        let address_ref = UserRef::<T>::new(address);
        current_task.write_object(address_ref, value)?;
        file.ioctl(locked, current_task, command, address.into())?;
        current_task.read_object(address_ref)
    }

    fn set_controlling_terminal(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        file: &FileHandle,
        steal: bool,
    ) -> Result<SyscallResult, Errno> {
        #[allow(clippy::bool_to_int_with_if)]
        file.ioctl(locked, current_task, TIOCSCTTY, steal.into())
    }

    fn lookup_node<L>(
        locked: &mut Locked<'_, L>,
        task: &CurrentTask,
        fs: &FileSystemHandle,
        name: &FsStr,
    ) -> Result<NamespaceNode, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        let root = NamespaceNode::new_anonymous(fs.root().clone());
        root.lookup_child(locked, task, &mut Default::default(), name)
    }

    fn open_file_with_flags(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        fs: &FileSystemHandle,
        name: &FsStr,
        flags: OpenFlags,
    ) -> Result<FileHandle, Errno> {
        let node = lookup_node(locked, current_task, fs, name)?;
        node.open(locked, current_task, flags, AccessCheck::default())
    }

    fn open_file(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        fs: &FileSystemHandle,
        name: &FsStr,
    ) -> Result<FileHandle, Errno> {
        open_file_with_flags(locked, current_task, fs, name, OpenFlags::RDWR | OpenFlags::NOCTTY)
    }

    fn open_ptmx_and_unlock(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        fs: &FileSystemHandle,
    ) -> Result<FileHandle, Errno> {
        let file = open_file_with_flags(locked, current_task, fs, "ptmx".into(), OpenFlags::RDWR)?;

        // Unlock terminal
        ioctl::<i32>(locked, current_task, &file, TIOCSPTLCK, &0)?;

        Ok(file)
    }

    #[::fuchsia::test]
    async fn opening_ptmx_creates_pts() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task);
        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        lookup_node(&mut locked, &task, &fs, "0".into()).unwrap_err();
        let _ptmx = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");
        lookup_node(&mut locked, &task, &fs, "0".into()).expect("pty");
    }

    #[::fuchsia::test]
    async fn closing_ptmx_closes_pts() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task);
        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        lookup_node(&mut locked, &task, &fs, "0".into()).unwrap_err();
        let ptmx = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");
        let _pts = open_file(&mut locked, &task, &fs, "0".into()).expect("open file");
        std::mem::drop(ptmx);
        task.trigger_delayed_releaser(&mut locked);
        lookup_node(&mut locked, &task, &fs, "0".into()).unwrap_err();
    }

    #[::fuchsia::test]
    async fn pts_are_reused() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task);
        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");

        let _ptmx0 = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");
        let mut _ptmx1 = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");
        let _ptmx2 = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");

        lookup_node(&mut locked, &task, &fs, "0".into()).expect("component_lookup");
        lookup_node(&mut locked, &task, &fs, "1".into()).expect("component_lookup");
        lookup_node(&mut locked, &task, &fs, "2".into()).expect("component_lookup");

        std::mem::drop(_ptmx1);
        task.trigger_delayed_releaser(&mut locked);

        lookup_node(&mut locked, &task, &fs, "1".into()).unwrap_err();

        _ptmx1 = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");
        lookup_node(&mut locked, &task, &fs, "1".into()).expect("component_lookup");
    }

    #[::fuchsia::test]
    async fn opening_inexistant_replica_fails() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task);
        // Initialize pts devices
        ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        let fs = TmpFs::new_fs(&kernel);
        let mount = MountInfo::detached();
        let pts = fs
            .root()
            .create_entry(
                &mut locked,
                &task,
                &mount,
                "custom_pts".into(),
                |locked, dir, mount, name| {
                    dir.mknod(
                        locked,
                        &task,
                        mount,
                        name,
                        mode!(IFCHR, 0o666),
                        DeviceType::new(DEVPTS_FIRST_MAJOR, 0),
                        FsCred::root(),
                    )
                },
            )
            .expect("custom_pts");
        let node = NamespaceNode::new_anonymous(pts.clone());
        assert!(node.open(&mut locked, &task, OpenFlags::RDONLY, AccessCheck::skip()).is_err());
    }

    #[::fuchsia::test]
    async fn test_open_tty() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task);
        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        let devfs = crate::fs::devtmpfs::DevTmpFs::from_task(&mut locked, &task);

        let ptmx = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");
        set_controlling_terminal(&mut locked, &task, &ptmx, false)
            .expect("set_controlling_terminal");
        let tty = open_file_with_flags(&mut locked, &task, &devfs, "tty".into(), OpenFlags::RDWR)
            .expect("tty");
        // Check that tty is the main terminal by calling the ioctl TIOCGPTN and checking it is
        // has the same result as on ptmx.
        assert_eq!(
            ioctl::<i32>(&mut locked, &task, &tty, TIOCGPTN, &0),
            ioctl::<i32>(&mut locked, &task, &ptmx, TIOCGPTN, &0)
        );

        // Detach the controlling terminal.
        ioctl::<i32>(&mut locked, &task, &ptmx, TIOCNOTTY, &0).expect("detach terminal");
        let pts = open_file(&mut locked, &task, &fs, "0".into()).expect("open file");
        set_controlling_terminal(&mut locked, &task, &pts, false)
            .expect("set_controlling_terminal");
        let tty = open_file_with_flags(&mut locked, &task, &devfs, "tty".into(), OpenFlags::RDWR)
            .expect("tty");
        // TIOCGPTN is not implemented on replica terminals
        assert!(ioctl::<i32>(&mut locked, &task, &tty, TIOCGPTN, &0).is_err());
    }

    #[::fuchsia::test]
    async fn test_unknown_ioctl() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task);
        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");

        let ptmx = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");
        assert_eq!(ptmx.ioctl(&mut locked, &task, 42, Default::default()), error!(ENOTTY));

        let pts_file = open_file(&mut locked, &task, &fs, "0".into()).expect("open file");
        assert_eq!(pts_file.ioctl(&mut locked, &task, 42, Default::default()), error!(ENOTTY));
    }

    #[::fuchsia::test]
    async fn test_tiocgptn_ioctl() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task);
        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        let ptmx0 = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");
        let ptmx1 = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");

        let pts0 = ioctl::<u32>(&mut locked, &task, &ptmx0, TIOCGPTN, &0).expect("ioctl");
        assert_eq!(pts0, 0);

        let pts1 = ioctl::<u32>(&mut locked, &task, &ptmx1, TIOCGPTN, &0).expect("ioctl");
        assert_eq!(pts1, 1);
    }

    #[::fuchsia::test]
    async fn test_new_terminal_is_locked() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task);
        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        let _ptmx_file = open_file(&mut locked, &task, &fs, "ptmx".into()).expect("open file");

        let pts = lookup_node(&mut locked, &task, &fs, "0".into()).expect("component_lookup");
        assert_eq!(
            pts.open(&mut locked, &task, OpenFlags::RDONLY, AccessCheck::default()).map(|_| ()),
            error!(EIO)
        );
    }

    #[::fuchsia::test]
    async fn test_lock_ioctls() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task);
        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        let ptmx = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");
        let pts = lookup_node(&mut locked, &task, &fs, "0".into()).expect("component_lookup");

        // Check that the lock is not set.
        assert_eq!(ioctl::<i32>(&mut locked, &task, &ptmx, TIOCGPTLCK, &0), Ok(0));
        // /dev/pts/0 can be opened
        pts.open(&mut locked, &task, OpenFlags::RDONLY, AccessCheck::default()).expect("open");

        // Lock the terminal
        ioctl::<i32>(&mut locked, &task, &ptmx, TIOCSPTLCK, &42).expect("ioctl");
        // Check that the lock is set.
        assert_eq!(ioctl::<i32>(&mut locked, &task, &ptmx, TIOCGPTLCK, &0), Ok(1));
        // /dev/pts/0 cannot be opened
        assert_eq!(
            pts.open(&mut locked, &task, OpenFlags::RDONLY, AccessCheck::default()).map(|_| ()),
            error!(EIO)
        );
    }

    #[::fuchsia::test]
    async fn test_ptmx_stats() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task);
        task.set_creds(Credentials::with_ids(22, 22));
        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        let ptmx = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");
        let ptmx_stat = ptmx.node().stat(&mut locked, &task).expect("stat");
        assert_eq!(ptmx_stat.st_blksize as usize, BLOCK_SIZE);
        let pts = open_file(&mut locked, &task, &fs, "0".into()).expect("open file");
        let pts_stats = pts.node().stat(&mut locked, &task).expect("stat");
        assert_eq!(pts_stats.st_mode & FileMode::PERMISSIONS.bits(), 0o620);
        assert_eq!(pts_stats.st_uid, 22);
        // TODO(qsr): Check that gid is tty.
    }

    #[::fuchsia::test]
    async fn test_attach_terminal_when_open() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task);
        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        let _opened_main = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");
        // Opening the main terminal should not set the terminal of the session.
        assert!(task
            .thread_group
            .read()
            .process_group
            .session
            .read()
            .controlling_terminal
            .is_none());
        // Opening the terminal should not set the terminal of the session with the NOCTTY flag.
        let _opened_replica2 = open_file_with_flags(
            &mut locked,
            &task,
            &fs,
            "0".into(),
            OpenFlags::RDWR | OpenFlags::NOCTTY,
        )
        .expect("open file");
        assert!(task
            .thread_group
            .read()
            .process_group
            .session
            .read()
            .controlling_terminal
            .is_none());

        // Opening the replica terminal should set the terminal of the session.
        let _opened_replica2 =
            open_file_with_flags(&mut locked, &task, &fs, "0".into(), OpenFlags::RDWR)
                .expect("open file");
        assert!(task
            .thread_group
            .read()
            .process_group
            .session
            .read()
            .controlling_terminal
            .is_some());
    }

    #[::fuchsia::test]
    async fn test_attach_terminal() {
        let (kernel, task1, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task1);
        let task2 = task1.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        task2.thread_group.setsid(&mut locked).expect("setsid");

        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        let opened_main = open_ptmx_and_unlock(&mut locked, &task1, &fs).expect("ptmx");
        let opened_replica = open_file(&mut locked, &task2, &fs, "0".into()).expect("open file");

        assert_eq!(ioctl::<i32>(&mut locked, &task1, &opened_main, TIOCGPGRP, &0), error!(ENOTTY));
        assert_eq!(
            ioctl::<i32>(&mut locked, &task2, &opened_replica, TIOCGPGRP, &0),
            error!(ENOTTY)
        );

        set_controlling_terminal(&mut locked, &task1, &opened_main, false).unwrap();
        assert_eq!(
            ioctl::<i32>(&mut locked, &task1, &opened_main, TIOCGPGRP, &0),
            Ok(task1.thread_group.read().process_group.leader)
        );
        assert_eq!(
            ioctl::<i32>(&mut locked, &task2, &opened_replica, TIOCGPGRP, &0),
            error!(ENOTTY)
        );

        // Cannot steal terminal using the replica.
        assert_eq!(
            set_controlling_terminal(&mut locked, &task2, &opened_replica, false),
            error!(EPERM)
        );
        assert_eq!(
            ioctl::<i32>(&mut locked, &task2, &opened_replica, TIOCGPGRP, &0),
            error!(ENOTTY)
        );
    }

    #[::fuchsia::test]
    async fn test_steal_terminal() {
        let (kernel, task1, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task1);
        task1.set_creds(Credentials::with_ids(1, 1));

        let task2 = task1.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));

        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        let _opened_main = open_ptmx_and_unlock(&mut locked, &task1, &fs).expect("ptmx");
        let wo_opened_replica = open_file_with_flags(
            &mut locked,
            &task1,
            &fs,
            "0".into(),
            OpenFlags::WRONLY | OpenFlags::NOCTTY,
        )
        .expect("open file");
        assert!(!wo_opened_replica.can_read());

        // FD must be readable for setting the terminal.
        assert_eq!(
            set_controlling_terminal(&mut locked, &task1, &wo_opened_replica, false),
            error!(EPERM)
        );

        let opened_replica = open_file(&mut locked, &task2, &fs, "0".into()).expect("open file");
        // Task must be session leader for setting the terminal.
        assert_eq!(
            set_controlling_terminal(&mut locked, &task2, &opened_replica, false),
            error!(EINVAL)
        );

        // Associate terminal to task1.
        set_controlling_terminal(&mut locked, &task1, &opened_replica, false)
            .expect("Associate terminal to task1");

        // One cannot associate a terminal to a process that has already one
        assert_eq!(
            set_controlling_terminal(&mut locked, &task1, &opened_replica, false),
            error!(EINVAL)
        );

        task2.thread_group.setsid(&mut locked).expect("setsid");

        // One cannot associate a terminal that is already associated with another process.
        assert_eq!(
            set_controlling_terminal(&mut locked, &task2, &opened_replica, false),
            error!(EPERM)
        );

        // One cannot steal a terminal without the CAP_SYS_ADMIN capacility
        assert_eq!(
            set_controlling_terminal(&mut locked, &task2, &opened_replica, true),
            error!(EPERM)
        );

        // One can steal a terminal with the CAP_SYS_ADMIN capacility
        task2.set_creds(Credentials::with_ids(0, 0));
        // But not without specifying that one wants to steal it.
        assert_eq!(
            set_controlling_terminal(&mut locked, &task2, &opened_replica, false),
            error!(EPERM)
        );
        set_controlling_terminal(&mut locked, &task2, &opened_replica, true)
            .expect("Associate terminal to task2");

        assert!(task1
            .thread_group
            .read()
            .process_group
            .session
            .read()
            .controlling_terminal
            .is_none());
    }

    #[::fuchsia::test]
    async fn test_set_foreground_process() {
        let (kernel, init, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &init);
        let task1 = init.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        task1.thread_group.setsid(&mut locked).expect("setsid");
        let task2 = task1.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        task2.thread_group.setpgid(&mut locked, &task2, 0).expect("setpgid");
        let task2_pgid = task2.thread_group.read().process_group.leader;

        assert_ne!(task2_pgid, task1.thread_group.read().process_group.leader);

        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        let _opened_main = open_ptmx_and_unlock(&mut locked, &init, &fs).expect("ptmx");
        let opened_replica = open_file(&mut locked, &task2, &fs, "0".into()).expect("open file");

        // Cannot change the foreground process group if the terminal is not the controlling
        // terminal
        assert_eq!(
            ioctl::<i32>(&mut locked, &task2, &opened_replica, TIOCSPGRP, &task2_pgid),
            error!(ENOTTY)
        );

        // Attach terminal to task1 and task2 session.
        set_controlling_terminal(&mut locked, &task1, &opened_replica, false).unwrap();
        // The foreground process group should be the one of task1
        assert_eq!(
            ioctl::<i32>(&mut locked, &task1, &opened_replica, TIOCGPGRP, &0),
            Ok(task1.thread_group.read().process_group.leader)
        );

        // Cannot change the foreground process group to a negative pid.
        assert_eq!(
            ioctl::<i32>(&mut locked, &task2, &opened_replica, TIOCSPGRP, &-1),
            error!(EINVAL)
        );

        // Cannot change the foreground process group to a invalid process group.
        assert_eq!(
            ioctl::<i32>(&mut locked, &task2, &opened_replica, TIOCSPGRP, &255),
            error!(ESRCH)
        );

        // Cannot change the foreground process group to a process group in another session.
        let init_pgid = init.thread_group.read().process_group.leader;
        assert_eq!(
            ioctl::<i32>(&mut locked, &task2, &opened_replica, TIOCSPGRP, &init_pgid),
            error!(EPERM)
        );

        // Changing the foreground process while being in background generates SIGTTOU and fails.
        assert_eq!(
            ioctl::<i32>(&mut locked, &task2, &opened_replica, TIOCSPGRP, &task2_pgid),
            error!(EINTR)
        );
        assert!(task2.read().has_signal_pending(SIGTTOU));

        // Set the foreground process to task2 process group
        ioctl::<i32>(&mut locked, &task1, &opened_replica, TIOCSPGRP, &task2_pgid).unwrap();

        // Check that the foreground process has been changed.
        let terminal = Arc::clone(
            &task1
                .thread_group
                .read()
                .process_group
                .session
                .read()
                .controlling_terminal
                .as_ref()
                .unwrap()
                .terminal,
        );
        assert_eq!(
            terminal
                .read()
                .controller
                .as_ref()
                .unwrap()
                .session
                .upgrade()
                .unwrap()
                .read()
                .get_foreground_process_group_leader(),
            task2_pgid
        );
    }

    #[::fuchsia::test]
    async fn test_detach_session() {
        let (kernel, task1, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task1);
        let task2 = task1.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        task2.thread_group.setsid(&mut locked).expect("setsid");

        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        let _opened_main = open_ptmx_and_unlock(&mut locked, &task1, &fs).expect("ptmx");
        let opened_replica = open_file(&mut locked, &task1, &fs, "0".into()).expect("open file");

        // Cannot detach the controlling terminal when none is attached terminal
        assert_eq!(
            ioctl::<i32>(&mut locked, &task1, &opened_replica, TIOCNOTTY, &0),
            error!(ENOTTY)
        );

        set_controlling_terminal(&mut locked, &task2, &opened_replica, false)
            .expect("set controlling terminal");

        // Cannot detach the controlling terminal when not the session leader.
        assert_eq!(
            ioctl::<i32>(&mut locked, &task1, &opened_replica, TIOCNOTTY, &0),
            error!(ENOTTY)
        );

        // Detach the terminal
        ioctl::<i32>(&mut locked, &task2, &opened_replica, TIOCNOTTY, &0).expect("detach terminal");
        assert!(task2
            .thread_group
            .read()
            .process_group
            .session
            .read()
            .controlling_terminal
            .is_none());
    }

    #[::fuchsia::test]
    async fn test_send_data_back_and_forth() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        tty_device_init(&mut locked, &task);
        let fs = ensure_devpts(&kernel, Default::default()).expect("create dev_pts_fs");
        let ptmx = open_ptmx_and_unlock(&mut locked, &task, &fs).expect("ptmx");
        let pts = open_file(&mut locked, &task, &fs, "0".into()).expect("open file");

        let has_data_ready_to_read = |locked: &mut Locked<'_, Unlocked>, fd: &FileHandle| {
            fd.query_events(locked, &task).expect("query_events").contains(FdEvents::POLLIN)
        };

        let write_and_assert = |locked: &mut Locked<'_, Unlocked>, fd: &FileHandle, data: &[u8]| {
            assert_eq!(
                fd.write(locked, &task, &mut VecInputBuffer::new(data)).expect("write"),
                data.len()
            );
        };

        let read_and_check = |locked: &mut Locked<'_, Unlocked>, fd: &FileHandle, data: &[u8]| {
            assert!(has_data_ready_to_read(locked, fd));
            let mut buffer = VecOutputBuffer::new(data.len() + 1);
            assert_eq!(fd.read(locked, &task, &mut buffer).expect("read"), data.len());
            assert_eq!(data, buffer.data());
        };

        let hello_buffer = b"hello\n";
        let hello_transformed_buffer = b"hello\r\n";

        // Main to replica
        write_and_assert(&mut locked, &ptmx, hello_buffer);
        read_and_check(&mut locked, &pts, hello_buffer);

        // Data has been echoed
        read_and_check(&mut locked, &ptmx, hello_transformed_buffer);

        // Replica to main
        write_and_assert(&mut locked, &pts, hello_buffer);
        read_and_check(&mut locked, &ptmx, hello_transformed_buffer);

        // Data has not been echoed
        assert!(!has_data_ready_to_read(&mut locked, &pts));
    }
}
