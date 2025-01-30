// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::container_namespace::ContainerNamespace;
use crate::device::android::bootloader_message_store::AndroidBootloaderMessageStore;
use crate::device::binder::BinderDevice;
use crate::device::framebuffer::{AspectRatio, Framebuffer};
use crate::device::remote_block_device::RemoteBlockDeviceRegistry;
use crate::device::{DeviceMode, DeviceRegistry};
use crate::execution::CrashReporter;
use crate::fs::nmfs::NetworkManagerHandle;
use crate::fs::proc::SystemLimits;
use crate::memory_attribution::MemoryAttributionManager;
use crate::mm::{FutexTable, SharedFutexKey};
use crate::power::SuspendResumeManagerHandle;
use crate::security;
use crate::task::{
    AbstractUnixSocketNamespace, AbstractVsockSocketNamespace, CurrentTask, HrTimerManager,
    HrTimerManagerHandle, IpTables, KernelStats, KernelThreads, NetstackDevices, PidTable,
    PsiProvider, StopState, Syslog, ThreadGroup, UtsNamespace, UtsNamespaceHandle,
};
use crate::vdso::vdso_loader::Vdso;
use crate::vfs::crypt_service::CryptService;
use crate::vfs::socket::{
    GenericMessage, GenericNetlink, NetlinkSenderReceiverProvider, NetlinkToClientSender,
    SocketAddress,
};
use crate::vfs::{
    DelayedReleaser, FileHandle, FileOps, FileSystemHandle, FsNode, FsString, Mounts,
    StaticDirectoryBuilder,
};
use bstr::BString;
use expando::Expando;
use fidl::endpoints::{
    create_endpoints, ClientEnd, ControlHandle, DiscoverableProtocolMarker, ProtocolMarker,
};
use fidl_fuchsia_component_runner::{ComponentControllerControlHandle, ComponentStopInfo};
use fidl_fuchsia_feedback::CrashReporterProxy;
use fidl_fuchsia_scheduler::RoleManagerSynchronousProxy;
use futures::FutureExt;
use linux_uapi::FSCRYPT_KEY_IDENTIFIER_SIZE;
use netlink::interfaces::InterfacesHandler;
use netlink::{Netlink, NETLINK_LOG_TAG};
use once_cell::sync::OnceCell;
use starnix_lifecycle::{AtomicU32Counter, AtomicU64Counter};
use starnix_logging::{log_debug, log_error, log_info, log_warn};
use starnix_sync::{
    DeviceOpen, KernelIpTables, KernelSwapFiles, LockBefore, Locked, Mutex, OrderedMutex,
    OrderedRwLock, RwLock,
};
use starnix_types::ownership::{TempRef, WeakRef};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::from_status_like_fdio;
use starnix_uapi::open_flags::OpenFlags;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use zx::AsHandleRef;
use {
    fidl_fuchsia_io as fio, fidl_fuchsia_memory_attribution as fattribution,
    fuchsia_async as fasync,
};

#[derive(Debug, Default, Clone)]
pub struct KernelFeatures {
    pub bpf_v2: bool,

    /// Whether the kernel supports the S_ISUID and S_ISGID bits.
    ///
    /// For example, these bits are used by `sudo`.
    ///
    /// Enabling this feature is potentially a security risk because they allow privilege
    /// escalation.
    pub enable_suid: bool,

    /// Whether io_uring is enabled.
    ///
    /// TODO(https://fxbug.dev/297431387): Enabled by default once the feature is completed.
    pub io_uring: bool,

    /// Whether the kernel should return an error to userspace, rather than panicking, if `reboot()`
    /// is requested but cannot be enacted because the kernel lacks the relevant capabilities.
    pub error_on_failed_reboot: bool,

    /// This controls whether or not the default framebuffer background is black or colorful, to
    /// aid debugging.
    pub enable_visual_debugging: bool,

    /// The default seclabel that is applied to components that are run in this kernel.
    ///
    /// Components can override this by setting the `seclabel` field in their program block.
    pub default_seclabel: Option<String>,

    /// The default fsseclabel that is applied to components that are run in this kernel.
    ///
    /// Components can override this by setting the `fsseclabel` field in their program block.
    pub default_fsseclabel: Option<String>,

    /// The default uid that is applied to components that are run in this kernel.
    ///
    /// Components can override this by setting the `uid` field in their program block.
    pub default_uid: u32,
}

/// Contains an fscrypt wrapping key id.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EncryptionKeyId([u8; FSCRYPT_KEY_IDENTIFIER_SIZE as usize]);

impl From<[u8; FSCRYPT_KEY_IDENTIFIER_SIZE as usize]> for EncryptionKeyId {
    fn from(buf: [u8; FSCRYPT_KEY_IDENTIFIER_SIZE as usize]) -> Self {
        Self(buf)
    }
}

impl EncryptionKeyId {
    pub fn as_raw(&self) -> [u8; FSCRYPT_KEY_IDENTIFIER_SIZE as usize] {
        self.0.clone()
    }
}

/// The shared, mutable state for the entire Starnix kernel.
///
/// The `Kernel` object holds all kernel threads, userspace tasks, and file system resources for a
/// single instance of the Starnix kernel. In production, there is one instance of this object for
/// the entire Starnix kernel. However, multiple instances of this object can be created in one
/// process during unit testing.
///
/// The structure of this object will likely need to evolve as we implement more namespacing and
/// isolation mechanisms, such as `namespaces(7)` and `pid_namespaces(7)`.
pub struct Kernel {
    /// The kernel threads running on behalf of this kernel.
    pub kthreads: KernelThreads,

    /// The features enabled for this kernel.
    pub features: KernelFeatures,

    /// The processes and threads running in this kernel, organized by pid_t.
    pub pids: RwLock<PidTable>,

    /// Subsystem-specific properties that hang off the Kernel object.
    ///
    /// Instead of adding yet another property to the Kernel object, consider storing the property
    /// in an expando if that property is only used by one part of the system, such as a module.
    pub expando: Expando,

    /// The default namespace for abstract AF_UNIX sockets in this kernel.
    ///
    /// Rather than use this default namespace, abstract socket addresses
    /// should be looked up in the AbstractSocketNamespace on each Task
    /// object because some Task objects might have a non-default namespace.
    pub default_abstract_socket_namespace: Arc<AbstractUnixSocketNamespace>,

    /// The default namespace for abstract AF_VSOCK sockets in this kernel.
    pub default_abstract_vsock_namespace: Arc<AbstractVsockSocketNamespace>,

    /// The kernel command line. Shows up in /proc/cmdline.
    pub cmdline: BString,

    // Owned by anon_node.rs
    pub anon_fs: OnceLock<FileSystemHandle>,
    // Owned by pipe.rs
    pub pipe_fs: OnceLock<FileSystemHandle>,
    // Owned by socket.rs
    pub socket_fs: OnceLock<FileSystemHandle>,
    // Owned by devtmpfs.rs
    pub dev_tmp_fs: OnceLock<FileSystemHandle>,
    // Owned by devpts.rs
    pub dev_pts_fs: OnceLock<FileSystemHandle>,
    // Owned by procfs.rs
    pub proc_fs: OnceLock<FileSystemHandle>,
    // Owned by sysfs.rs
    pub sys_fs: OnceLock<FileSystemHandle>,
    // Owned by security/selinux_hooks/fs.rs
    pub selinux_fs: OnceCell<FileSystemHandle>,
    // Owned by nmfs.rs
    pub nmfs: OnceLock<FileSystemHandle>,
    // Global state held by the Linux Security Modules subsystem.
    pub security_state: security::KernelState,
    // Owned by tracefs/fs.rs
    pub trace_fs: OnceLock<FileSystemHandle>,

    /// The registry of device drivers.
    pub device_registry: DeviceRegistry,

    /// Mapping of top-level namespace entries to an associated proxy.
    /// For example, "/svc" to the respective proxy. Only the namespace entries
    /// which were known at component startup will be available by the kernel.
    pub container_namespace: ContainerNamespace,

    /// The registry of block devices backed by a remote fuchsia.io file.
    pub remote_block_device_registry: Arc<RemoteBlockDeviceRegistry>,

    /// If a remote block device named "misc" is created, keep track of it; this is used by Android
    /// to pass boot parameters to the bootloader.  Since Starnix is acting as a de-facto bootloader
    /// for Android, we need to be able to peek into these messages.
    /// Note that this might never be initialized (if the "misc" device never gets registered).
    pub bootloader_message_store: OnceLock<AndroidBootloaderMessageStore>,

    /// A `Framebuffer` that can be used to display a view in the workstation UI. If the container
    /// specifies the `framebuffer` feature this framebuffer will be registered as a device.
    ///
    /// When a component is run in that container and also specifies the `framebuffer` feature, the
    /// framebuffer will be served as the view of the component.
    pub framebuffer: Arc<Framebuffer>,

    /// The binder driver registered for this container, indexed by their device type.
    pub binders: RwLock<BTreeMap<DeviceType, BinderDevice>>,

    /// The iptables used for filtering network packets.
    pub iptables: OrderedRwLock<IpTables, KernelIpTables>,

    /// The futexes shared across processes.
    pub shared_futexes: FutexTable<SharedFutexKey>,

    /// The default UTS namespace for all tasks.
    ///
    /// Because each task can have its own UTS namespace, you probably want to use
    /// the UTS namespace handle of the task, which may/may not point to this one.
    pub root_uts_ns: UtsNamespaceHandle,

    /// A struct containing a VMO with a vDSO implementation, if implemented for a given architecture, and possibly an offset for a sigreturn function.
    pub vdso: Vdso,

    /// A struct containing a VMO with a arch32-vDSO implementation, if implemented for a given architecture.
    // TODO(https://fxbug.dev/380431743) This could be made less clunky -- maybe a Vec<Vdso> above or
    // something else
    pub vdso_arch32: Option<Vdso>,

    /// The table of devices installed on the netstack and their associated
    /// state local to this `Kernel`.
    pub netstack_devices: Arc<NetstackDevices>,

    /// Files that are currently available for swapping.
    /// Note: Starnix never actually swaps memory to these files. We just need to track them
    /// to pass conformance tests.
    pub swap_files: OrderedMutex<Vec<FileHandle>, KernelSwapFiles>,

    /// The implementation of generic Netlink protocol families.
    generic_netlink: OnceLock<GenericNetlink<NetlinkToClientSender<GenericMessage>>>,

    /// The implementation of networking-related Netlink protocol families.
    network_netlink: OnceLock<Netlink<NetlinkSenderReceiverProvider>>,

    /// Inspect instrumentation for this kernel instance.
    pub inspect_node: fuchsia_inspect::Node,

    /// The kinds of seccomp action that gets logged, stored as a bit vector.
    /// Each potential SeccompAction gets a bit in the vector, as specified by
    /// SeccompAction::logged_bit_offset.  If the bit is set, that means the
    /// action should be logged when it is taken, subject to the caveats
    /// described in seccomp(2).  The value of the bit vector is exposed to users
    /// in a text form in the file /proc/sys/kernel/seccomp/actions_logged.
    pub actions_logged: AtomicU16,

    /// The manager for suspend/resume.
    pub suspend_resume_manager: SuspendResumeManagerHandle,

    /// The manager for communicating network property updates
    /// to the network policy proxy.
    pub network_manager: NetworkManagerHandle,

    /// Unique IDs for new mounts and mount namespaces.
    pub next_mount_id: AtomicU64Counter,
    pub next_peer_group_id: AtomicU64Counter,
    pub next_namespace_id: AtomicU64Counter,

    /// Unique IDs for file objects.
    pub next_file_object_id: AtomicU64Counter,

    /// Unique cookie used to link two inotify events, usually an IN_MOVE_FROM/IN_MOVE_TO pair.
    pub next_inotify_cookie: AtomicU32Counter,

    /// Controls which processes a process is allowed to ptrace.  See Documentation/security/Yama.txt
    pub ptrace_scope: AtomicU8,

    // The Fuchsia build version returned by `fuchsia.buildinfo.Provider`.
    pub build_version: OnceCell<String>,

    pub stats: Arc<KernelStats>,

    // Proxy to the PSI provider we received from the runner, if any.
    pub psi_provider: PsiProvider,

    /// Resource limits that are exposed, for example, via sysctl.
    pub system_limits: SystemLimits,

    // The service to handle delayed releases. This is required for elements that requires to
    // execute some code when released and requires a known context (both in term of lock context,
    // as well as `CurrentTask`).
    pub delayed_releaser: DelayedReleaser,

    /// Proxy to the scheduler role manager for adjusting task priorities.
    pub role_manager: Option<RoleManagerSynchronousProxy>,

    /// The syslog manager.
    pub syslog: Syslog,

    /// All mounts.
    pub mounts: Mounts,

    /// The manager for creating and managing high-resolution timers.
    pub hrtimer_manager: HrTimerManagerHandle,

    /// The manager for monitoring and reporting resources used by the kernel.
    pub memory_attribution_manager: MemoryAttributionManager,

    /// Handler for crashing Linux processes.
    pub crash_reporter: CrashReporter,

    /// Implements the fuchsia.fxfs.Crypt protocol. Maintains an internal structure that maps each
    /// encryption key id to both the set of users that have added that key and the key-derived
    /// cipher.
    pub crypt_service: Arc<CryptService>,

    /// Vector of functions to be run when procfs is constructed. This is to allow
    /// modules to expose directories into /proc/device-tree.
    pub procfs_device_tree_setup: Vec<fn(&mut StaticDirectoryBuilder<'_>, &CurrentTask)>,

    /// Whether this kernel is shutting down. When shutting down, new processes may not be spawned.
    shutting_down: AtomicBool,

    /// Control handle to the running container's ComponentController.
    pub container_control_handle: Mutex<Option<ComponentControllerControlHandle>>,
}

/// An implementation of [`InterfacesHandler`].
///
/// This holds a `Weak<Kernel>` because it is held within a [`Netlink`] which
/// is itself held within an `Arc<Kernel>`. Holding an `Arc<T>` within an
/// `Arc<T>` prevents the `Arc`'s ref count from ever reaching 0, causing a
/// leak.
struct InterfacesHandlerImpl(Weak<Kernel>);

impl InterfacesHandlerImpl {
    fn with_netstack_devices<
        F: FnOnce(
                &CurrentTask,
                &Arc<NetstackDevices>,
                Option<&FileSystemHandle>,
                Option<&FileSystemHandle>,
            ) + Sync
            + Send
            + 'static,
    >(
        &mut self,
        f: F,
    ) {
        if let Some(kernel) = self.0.upgrade() {
            kernel.kthreads.spawner().spawn(move |_, current_task| {
                let kernel = current_task.kernel();
                f(current_task, &kernel.netstack_devices, kernel.proc_fs.get(), kernel.sys_fs.get())
            });
        }
    }
}

impl InterfacesHandler for InterfacesHandlerImpl {
    fn handle_new_link(&mut self, name: &str) {
        let name = name.to_owned();
        self.with_netstack_devices(move |current_task, devs, proc_fs, sys_fs| {
            devs.add_dev(current_task, &name, proc_fs, sys_fs)
        })
    }

    fn handle_deleted_link(&mut self, name: &str) {
        let name = name.to_owned();
        self.with_netstack_devices(move |_current_task, devs, _proc_fs, _sys_fs| {
            devs.remove_dev(&name)
        })
    }
}

impl Kernel {
    pub fn new(
        cmdline: BString,
        features: KernelFeatures,
        container_namespace: ContainerNamespace,
        role_manager: Option<RoleManagerSynchronousProxy>,
        crash_reporter_proxy: Option<CrashReporterProxy>,
        inspect_node: fuchsia_inspect::Node,
        framebuffer_aspect_ratio: Option<&AspectRatio>,
        security_state: security::KernelState,
        procfs_device_tree_setup: Vec<fn(&mut StaticDirectoryBuilder<'_>, &CurrentTask)>,
    ) -> Result<Arc<Kernel>, zx::Status> {
        let unix_address_maker =
            Box::new(|x: FsString| -> SocketAddress { SocketAddress::Unix(x) });
        let vsock_address_maker = Box::new(|x: u32| -> SocketAddress { SocketAddress::Vsock(x) });
        let framebuffer =
            Framebuffer::new(framebuffer_aspect_ratio, features.enable_visual_debugging)
                .expect("Failed to create framebuffer");

        let crash_reporter = CrashReporter::new(&inspect_node, crash_reporter_proxy);
        let network_manager = NetworkManagerHandle::new_with_inspect(&inspect_node);
        let hrtimer_manager = HrTimerManager::new(&inspect_node);

        let this = Arc::new_cyclic(|kernel| Kernel {
            kthreads: KernelThreads::new(kernel.clone()),
            features,
            pids: RwLock::new(PidTable::new()),
            expando: Default::default(),
            default_abstract_socket_namespace: AbstractUnixSocketNamespace::new(unix_address_maker),
            default_abstract_vsock_namespace: AbstractVsockSocketNamespace::new(
                vsock_address_maker,
            ),
            cmdline,
            anon_fs: Default::default(),
            pipe_fs: Default::default(),
            dev_tmp_fs: Default::default(),
            dev_pts_fs: Default::default(),
            proc_fs: Default::default(),
            socket_fs: Default::default(),
            sys_fs: Default::default(),
            selinux_fs: Default::default(),
            nmfs: Default::default(),
            security_state,
            trace_fs: Default::default(),
            device_registry: Default::default(),
            container_namespace,
            remote_block_device_registry: Default::default(),
            bootloader_message_store: OnceLock::new(),
            framebuffer,
            binders: Default::default(),
            iptables: OrderedRwLock::new(IpTables::new()),
            shared_futexes: FutexTable::<SharedFutexKey>::default(),
            root_uts_ns: Arc::new(RwLock::new(UtsNamespace::default())),
            vdso: Vdso::new(),
            vdso_arch32: Vdso::new_arch32(),
            netstack_devices: Arc::default(),
            swap_files: Default::default(),
            generic_netlink: OnceLock::new(),
            network_netlink: OnceLock::new(),
            inspect_node,
            actions_logged: AtomicU16::new(0),
            suspend_resume_manager: Default::default(),
            network_manager,
            next_mount_id: AtomicU64Counter::new(1),
            next_peer_group_id: AtomicU64Counter::new(1),
            next_namespace_id: AtomicU64Counter::new(1),
            next_inotify_cookie: AtomicU32Counter::new(1),
            next_file_object_id: Default::default(),
            system_limits: SystemLimits::default(),
            ptrace_scope: AtomicU8::new(0),
            build_version: OnceCell::new(),
            stats: Arc::new(KernelStats::default()),
            psi_provider: PsiProvider::default(),
            delayed_releaser: Default::default(),
            role_manager,
            syslog: Default::default(),
            mounts: Mounts::new(),
            hrtimer_manager,
            memory_attribution_manager: MemoryAttributionManager::new(kernel.clone()),
            crash_reporter,
            crypt_service: Arc::new(CryptService::new()),
            procfs_device_tree_setup,
            shutting_down: AtomicBool::new(false),
            container_control_handle: Mutex::new(None),
        });

        // Make a copy of this Arc for the inspect lazy node to use but don't create an Arc cycle
        // because the inspect node that owns this reference is owned by the kernel.
        let kernel = Arc::downgrade(&this);
        this.inspect_node.record_lazy_child("thread_groups", move || {
            if let Some(kernel) = kernel.upgrade() {
                let inspector = kernel.get_thread_groups_inspect();
                async move { Ok(inspector) }.boxed()
            } else {
                async move { Err(anyhow::format_err!("kernel was dropped")) }.boxed()
            }
        });

        Ok(this)
    }

    /// Shuts down userspace and the kernel in an orderly fashion, eventually terminating the root
    /// kernel process.
    pub fn shut_down(self: &Arc<Self>) {
        // Run shutdown code on a kthread in the main process so that it can be the last process
        // alive.
        let kernel = self.clone();
        self.kthreads.spawn_future(async move {
            kernel.run_shutdown().await;
        });
    }

    /// Starts shutting down the Starnix kernel and any running container. Only one thread can drive
    /// shutdown at a time. This function will return immediately if shut down is already under way.
    ///
    /// Shutdown happens in several phases:
    ///
    /// 1. Disable launching new processes
    /// 2. Shut down individual ThreadGroups until only the init and system tasks remain
    /// 3. Repeat the above for the init task
    /// 4. Ensure this process is the only one running in the kernel job.
    /// 5. Tell CF the container component has stopped
    /// 6. Exit this process
    ///
    /// If a ThreadGroup does not shut down on its own (including after SIGKILL), that phase of
    /// shutdown will hang. To gracefully shut down any further we need the other kernel processes
    /// to do controlled exits that properly release access to shared state. If our orderly shutdown
    /// does hang, eventually CF will kill the container component which will lead to the job of
    /// this process being killed and shutdown will still complete.
    async fn run_shutdown(&self) {
        const INIT_PID: i32 = 1;
        const SYSTEM_TASK_PID: i32 = 2;

        // Step 1: Prevent new processes from being created once they observe this update. We don't
        // want the thread driving shutdown to be racing with other threads creating new processes.
        if self
            .shutting_down
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            log_debug!("Additional thread tried to initiate shutdown while already in-progress.");
            return;
        }

        log_info!("Shutting down Starnix kernel.");

        // Step 2: Shut down thread groups in a loop until init and the system task are all that
        // remain.
        loop {
            let tgs = {
                // Exiting thread groups need to acquire a write lock for the pid table to
                // successfully exit so we need to acquire that lock in a reduced scope.
                self.pids
                    .read()
                    .get_thread_groups()
                    .filter(|tg| tg.leader != SYSTEM_TASK_PID && tg.leader != INIT_PID)
                    .map(TempRef::into_static)
                    .collect::<Vec<_>>()
            };
            if tgs.is_empty() {
                log_debug!("pid table is empty except init and system task");
                break;
            }

            log_debug!(tgs:?; "shutting down thread groups");
            let mut tasks = vec![];
            for tg in tgs {
                let task = fasync::Task::local(ThreadGroup::shut_down(WeakRef::from(tg)));
                tasks.push(task);
            }
            futures::future::join_all(tasks).await;
        }

        // Step 3: Terminate the init process.
        let maybe_init = {
            // Exiting thread groups need to acquire a write lock for the pid table to successfully
            // exit so we need to acquire that lock in a reduced scope.
            self.pids.read().get_thread_group(1).map(|tg| WeakRef::from(tg))
        };
        if let Some(init) = maybe_init {
            log_debug!("shutting down init");
            ThreadGroup::shut_down(init).await;
        } else {
            log_debug!("init already terminated");
        }

        // Step 4: Make sure this is the only process running in the job. We already should have
        // cleared up all processes other than the system task at this point, but wait on any that
        // might be around for good measure.
        //
        // Use unwrap liberally since we're shutting down anyway and errors will still tear down the
        // kernel.
        let kernel_job = fuchsia_runtime::job_default();
        assert_eq!(kernel_job.children().unwrap(), &[], "starnix does not create any child jobs");
        let own_koid = fuchsia_runtime::process_self().get_koid().unwrap();

        log_debug!("waiting for this to be the only process in the job");
        loop {
            let remaining_processes = kernel_job.processes().unwrap();
            if remaining_processes.len() == 1 && remaining_processes[0] == own_koid {
                log_debug!("No stray Zircon processes.");
                break;
            }

            let mut terminated_signals = vec![];
            for koid in remaining_processes {
                // Don't wait for ourselves to exit.
                if koid == own_koid {
                    continue;
                }
                let handle = match kernel_job.get_child(
                    &koid,
                    zx::Rights::BASIC | zx::Rights::PROPERTY | zx::Rights::DESTROY,
                ) {
                    Ok(h) => h,
                    Err(e) => {
                        log_debug!(koid:?, e:?; "failed to get child process from job");
                        continue;
                    }
                };
                terminated_signals
                    .push(fuchsia_async::OnSignals::new(handle, zx::Signals::PROCESS_TERMINATED));
            }
            log_debug!("waiting on process terminated signals");
            futures::future::join_all(terminated_signals).await;
        }

        // Step 5: Tell CF the container stopped.
        log_debug!("all non-root processes killed, notifying CF container is stopped");
        if let Some(control_handle) = self.container_control_handle.lock().take() {
            log_debug!("Notifying CF that the container has stopped.");
            control_handle
                .send_on_stop(ComponentStopInfo {
                    termination_status: Some(zx::Status::OK.into_raw()),
                    exit_code: Some(0),
                    ..ComponentStopInfo::default()
                })
                .unwrap();
            control_handle.shutdown_with_epitaph(zx::Status::OK);
        } else {
            log_warn!("Shutdown invoked without a container controller control handle.");
        }

        // Step 6: exiting this process.
        log_info!("All tasks killed, exiting Starnix kernel root process.");
        std::process::exit(0);
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    /// Opens a device file (driver) identified by `dev`.
    pub fn open_device<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        node: &FsNode,
        flags: OpenFlags,
        dev: DeviceType,
        mode: DeviceMode,
    ) -> Result<Box<dyn FileOps>, Errno>
    where
        L: LockBefore<DeviceOpen>,
    {
        self.device_registry.open_device(locked, current_task, node, flags, dev, mode)
    }

    /// Return a reference to the GenericNetlink implementation.
    ///
    /// This function follows the lazy initialization pattern, where the first
    /// call will instantiate the Generic Netlink server in a separate kthread.
    pub(crate) fn generic_netlink(&self) -> &GenericNetlink<NetlinkToClientSender<GenericMessage>> {
        self.generic_netlink.get_or_init(|| {
            let (generic_netlink, generic_netlink_fut) = GenericNetlink::new();
            self.kthreads.spawn_future(async move {
                generic_netlink_fut.await;
                log_error!("Generic Netlink future unexpectedly exited");
            });
            generic_netlink
        })
    }

    /// Return a reference to the [`netlink::Netlink`] implementation.
    ///
    /// This function follows the lazy initialization pattern, where the first
    /// call will instantiate the Netlink implementation.
    pub(crate) fn network_netlink<'a>(
        self: &'a Arc<Self>,
    ) -> &'a Netlink<NetlinkSenderReceiverProvider> {
        self.network_netlink.get_or_init(|| {
            let (network_netlink, network_netlink_async_worker) =
                Netlink::new(InterfacesHandlerImpl(Arc::downgrade(self)));
            self.kthreads.spawn(move |_, _| {
                fasync::LocalExecutor::new().run_singlethreaded(network_netlink_async_worker);
                log_error!(tag = NETLINK_LOG_TAG; "Netlink async worker unexpectedly exited");
            });
            network_netlink
        })
    }

    /// Returns a Proxy to the service used by the container at `filename`.
    #[allow(unused)]
    pub fn connect_to_named_protocol_at_container_svc<P: ProtocolMarker>(
        &self,
        filename: &str,
    ) -> Result<ClientEnd<P>, Errno> {
        match self.container_namespace.get_namespace_channel("/svc") {
            Ok(channel) => {
                let (client_end, server_end) = create_endpoints::<P>();
                fdio::service_connect_at(channel.as_ref(), filename, server_end.into_channel())
                    .map_err(|status| from_status_like_fdio!(status))?;
                Ok(client_end)
            }
            Err(err) => {
                log_error!("Unable to get /svc namespace channel! {}", err);
                Err(Errno::new(starnix_uapi::errors::ENOENT))
            }
        }
    }

    /// Returns a Proxy to the service `P` used by the container.
    pub fn connect_to_protocol_at_container_svc<P: DiscoverableProtocolMarker>(
        &self,
    ) -> Result<ClientEnd<P>, Errno> {
        self.connect_to_named_protocol_at_container_svc::<P>(P::PROTOCOL_NAME)
    }

    fn get_thread_groups_inspect(&self) -> fuchsia_inspect::Inspector {
        let inspector = fuchsia_inspect::Inspector::default();

        let thread_groups = inspector.root();
        for thread_group in self.pids.read().get_thread_groups() {
            let tg = thread_group.read();

            let tg_node = thread_groups.create_child(format!("{}", thread_group.leader));
            if let Ok(koid) = &thread_group.process.get_koid() {
                tg_node.record_int("koid", koid.raw_koid() as i64);
            }
            tg_node.record_int("pid", thread_group.leader as i64);
            tg_node.record_int("ppid", tg.get_ppid() as i64);
            tg_node.record_bool("stopped", thread_group.load_stopped() == StopState::GroupStopped);

            let tasks_node = tg_node.create_child("tasks");
            for task in tg.tasks() {
                let set_properties = |node: &fuchsia_inspect::Node| {
                    node.record_string("command", task.command().to_str().unwrap_or("{err}"));

                    let sched_policy = task.read().scheduler_policy;
                    if !sched_policy.is_default() {
                        node.record_string("sched_policy", format!("{sched_policy:?}"));
                    }
                };
                if task.id == thread_group.leader {
                    set_properties(&tg_node);
                } else {
                    tasks_node.record_child(task.id.to_string(), |task_node| {
                        set_properties(task_node);
                    });
                };
            }
            tg_node.record(tasks_node);
            thread_groups.record(tg_node);
        }

        inspector
    }

    pub fn new_memory_attribution_observer(
        &self,
        control_handle: fattribution::ProviderControlHandle,
    ) -> attribution_server::Observer {
        self.memory_attribution_manager.new_observer(control_handle)
    }

    /// Opens and returns a directory proxy from the container's namespace, at
    /// the requested path, using the provided flags. This method will open the
    /// closest existing path from the namespace hierarchy. For instance, if
    /// the parameter provided is `/path/to/foo/bar` and the exists namespace
    /// entries for `/path/to/foo` and `/path/to`, then the former will be used
    /// as the root proxy and the subdir `bar` returned.
    pub fn open_ns_dir(
        &self,
        path: &str,
        open_flags: fio::Flags,
    ) -> Result<(fio::DirectorySynchronousProxy, String), Errno> {
        let ns_path = match path {
            // TODO(379929394): This condition is specifically to soft
            // transition the fstab file to the new format.
            "" | "/" | "." => PathBuf::from("/data"),
            _ => PathBuf::from(path),
        };

        match self.container_namespace.find_closest_channel(&ns_path) {
            Ok((root_channel, remaining_subdir)) => {
                let (_, server_end) = create_endpoints::<fio::DirectoryMarker>();
                fdio::open_at(
                    &root_channel,
                    &remaining_subdir,
                    open_flags,
                    server_end.into_channel(),
                )
                .map_err(|e| {
                    log_error!("Failed to intialize the subdirs: {}", e);
                    Errno::new(starnix_uapi::errors::EIO)
                })?;

                Ok((fio::DirectorySynchronousProxy::new(root_channel), remaining_subdir))
            }
            Err(err) => {
                log_error!(
                    "Unable to find a channel for {}. Received error: {}",
                    ns_path.display(),
                    err
                );
                Err(Errno::new(starnix_uapi::errors::ENOENT))
            }
        }
    }
}

impl std::fmt::Debug for Kernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kernel").finish()
    }
}
