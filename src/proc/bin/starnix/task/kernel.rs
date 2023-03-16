// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, Process};
use once_cell::sync::OnceCell;
use std::collections::{BTreeMap, HashSet};
use std::iter::FromIterator;
use std::sync::Arc;

use crate::device::framebuffer::Framebuffer;
use crate::device::{BinderDriver, DeviceMode, DeviceRegistry};
use crate::dynamic_thread_pool::DynamicThreadPool;
use crate::fs::file_server::FileServer;
use crate::fs::socket::SocketAddress;
use crate::fs::{FileOps, FileSystemHandle, FsNode};
use crate::lock::RwLock;
use crate::logging::set_zx_name;
use crate::mm::FutexTable;
use crate::task::*;
#[cfg(target_arch = "x86_64")]
use crate::types::*;
use crate::types::{DeviceType, Errno, OpenFlags};

pub struct Kernel {
    /// The Zircon job object that holds the processes running in this kernel.
    pub job: zx::Job,

    /// The main starnix process. This process is used to create new processes when using the
    /// restricted executor.
    pub starnix_process: Process,

    /// The processes and threads running in this kernel, organized by pid_t.
    pub pids: RwLock<PidTable>,

    /// The default namespace for abstract AF_UNIX sockets in this kernel.
    ///
    /// Rather than use this default namespace, abstract socket addresses
    /// should be looked up in the AbstractSocketNamespace on each Task
    /// object because some Task objects might have a non-default namespace.
    pub default_abstract_socket_namespace: Arc<AbstractUnixSocketNamespace>,

    /// The default namespace for abstract AF_VSOCK sockets in this kernel.
    pub default_abstract_vsock_namespace: Arc<AbstractVsockSocketNamespace>,

    /// The kernel command line. Shows up in /proc/cmdline.
    pub cmdline: Vec<u8>,

    // Owned by anon_node.rs
    pub anon_fs: OnceCell<FileSystemHandle>,
    // Owned by pipe.rs
    pub pipe_fs: OnceCell<FileSystemHandle>,
    /// Owned by socket.rs
    pub socket_fs: OnceCell<FileSystemHandle>,
    // Owned by devtmpfs.rs
    pub dev_tmp_fs: OnceCell<FileSystemHandle>,
    // Owned by devpts.rs
    pub dev_pts_fs: OnceCell<FileSystemHandle>,
    // Owned by procfs.rs
    pub proc_fs: OnceCell<FileSystemHandle>,
    // Owned by sysfs.rs
    pub sys_fs: OnceCell<FileSystemHandle>,
    // Owned by selinux.rs
    pub selinux_fs: OnceCell<FileSystemHandle>,

    /// The registry of device drivers.
    pub device_registry: RwLock<DeviceRegistry>,

    // The features enabled for the container this kernel is associated with, as specified in
    // the container's configuration file.
    pub features: HashSet<String>,

    /// A `Framebuffer` that can be used to display a view in the workstation UI. If the container
    /// specifies the `framebuffer` feature this framebuffer will be registered as a device.
    ///
    /// When a component is run in that container and also specifies the `framebuffer` feature, the
    /// framebuffer will be served as the view of the component.
    pub framebuffer: Arc<Framebuffer>,

    /// The binder driver registered for this container, indexed by their device type.
    pub binders: RwLock<BTreeMap<DeviceType, Arc<BinderDriver>>>,

    /// The iptables used for filtering network packets.
    pub iptables: RwLock<IpTables>,

    /// The futexes shared across processes.
    pub shared_futexes: FutexTable,

    /// The server allowing starnix to expose file and directory to fuchsia component.
    pub file_server: FileServer,

    /// The thread pool to dispatch blocking calls to.
    pub thread_pool: DynamicThreadPool,

    /// The default UTS namespace for all tasks.
    ///
    /// Because each task can have its own UTS namespace, you probably want to use
    /// the UTS namespace handle of the task, which may/may not point to this one.
    pub root_uts_ns: UtsNamespaceHandle,

    /// A VMO containing a vDSO implementation, if implemented for a given architecture.
    pub vdso: Option<Arc<zx::Vmo>>,
}

// TODO(fxbug.dev/121659): This function is only used to load the vDSO, and since only x64 has one
// this function is considered unused on other architectures
#[cfg(target_arch = "x86_64")]
fn sync_open_in_namespace(
    path: &str,
    flags: fidl_fuchsia_io::OpenFlags,
) -> Result<fidl_fuchsia_io::DirectorySynchronousProxy, Errno> {
    let (client, server) = fidl::Channel::create();
    let dir_proxy = fidl_fuchsia_io::DirectorySynchronousProxy::new(client);

    let namespace = fdio::Namespace::installed().map_err(|_| errno!(EINVAL))?;
    namespace.open(path, flags, server).map_err(|_| errno!(ENOENT))?;
    Ok(dir_proxy)
}

/// For the architectures for which a vDSO is implemented, read it from file and return a backing VMO.
#[cfg(target_arch = "x86_64")]
fn load_vdso_from_file() -> Result<Option<Arc<zx::Vmo>>, Errno> {
    const VDSO_FILENAME: &str = "libvdso.so";
    const VDSO_LOCATION: &str = "/pkg/data";

    let dir_proxy = sync_open_in_namespace(
        VDSO_LOCATION,
        fuchsia_fs::OpenFlags::RIGHT_READABLE | fuchsia_fs::OpenFlags::RIGHT_EXECUTABLE,
    )?;
    let vdso_vmo = syncio::directory_open_vmo(
        &dir_proxy,
        VDSO_FILENAME,
        fidl_fuchsia_io::VmoFlags::READ | fidl_fuchsia_io::VmoFlags::EXECUTE,
        zx::Time::INFINITE,
    )
    .map_err(|status| from_status_like_fdio!(status))?;
    Ok(Some(Arc::new(vdso_vmo)))
}

#[cfg(not(target_arch = "x86_64"))]
fn load_vdso_from_file() -> Result<Option<Arc<zx::Vmo>>, Errno> {
    Ok(None)
}

impl Kernel {
    pub fn new(
        name: &[u8],
        cmdline: &[u8],
        features: &[String],
    ) -> Result<Arc<Kernel>, zx::Status> {
        let unix_address_maker = Box::new(|x: Vec<u8>| -> SocketAddress { SocketAddress::Unix(x) });
        let vsock_address_maker = Box::new(|x: u32| -> SocketAddress { SocketAddress::Vsock(x) });
        let job = fuchsia_runtime::job_default().create_child_job()?;
        set_zx_name(&job, name);

        Ok(Arc::<Kernel>::new_cyclic(|kernel| Kernel {
            job,
            starnix_process: fuchsia_runtime::process_self()
                .duplicate(zx::Rights::SAME_RIGHTS)
                .expect("Failed to duplicate process self"),
            pids: RwLock::new(PidTable::new()),
            default_abstract_socket_namespace: AbstractUnixSocketNamespace::new(unix_address_maker),
            default_abstract_vsock_namespace: AbstractVsockSocketNamespace::new(
                vsock_address_maker,
            ),
            cmdline: cmdline.to_vec(),
            anon_fs: OnceCell::new(),
            pipe_fs: OnceCell::new(),
            dev_tmp_fs: OnceCell::new(),
            dev_pts_fs: OnceCell::new(),
            proc_fs: OnceCell::new(),
            socket_fs: OnceCell::new(),
            sys_fs: OnceCell::new(),
            selinux_fs: OnceCell::new(),
            device_registry: RwLock::new(DeviceRegistry::new_with_common_devices()),
            features: HashSet::from_iter(features.iter().cloned()),
            framebuffer: Framebuffer::new().expect("Failed to create framebuffer"),
            binders: Default::default(),
            iptables: RwLock::new(IpTables::new()),
            shared_futexes: Default::default(),
            file_server: FileServer::new(kernel.clone()),
            thread_pool: DynamicThreadPool::new(2),
            root_uts_ns: Arc::new(RwLock::new(UtsNamespace::default())),
            vdso: load_vdso_from_file().expect("Couldn't read vDSO from disk"),
        }))
    }

    /// Opens a device file (driver) identified by `dev`.
    pub fn open_device(
        &self,
        current_task: &CurrentTask,
        node: &FsNode,
        flags: OpenFlags,
        dev: DeviceType,
        mode: DeviceMode,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let registry = self.device_registry.read();
        registry.open_device(current_task, node, flags, dev, mode)
    }

    pub fn selinux_enabled(&self) -> bool {
        self.features.contains("selinux_enabled")
    }
}
