// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod permission_check;
pub mod policy;
pub mod security_server;

pub use security_server::SecurityServer;

mod access_vector_cache;
mod exceptions_config;
mod fifo_cache;
mod sid_table;
mod sync;

use policy::arrays::FsUseType;

use std::num::NonZeroU32;

/// Numeric class Ids are provided to the userspace AVC surfaces (e.g. "create", "access", etc).
pub use policy::ClassId;

/// The Security ID (SID) used internally to refer to a security context.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct SecurityId(NonZeroU32);

impl SecurityId {
    /// Returns a `SecurityId` encoding the specified initial Security Context.
    /// These are used when labeling kernel resources created before policy
    /// load, allowing the policy to determine the Security Context to use.
    pub fn initial(initial_sid: InitialSid) -> Self {
        Self(NonZeroU32::new(initial_sid as u32).unwrap())
    }
}

/// Identifies a specific class by its policy-defined Id, or as a kernel object class enum Id.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ObjectClass {
    /// Refers to a well-known SELinux kernel object class (e.g. "process", "file", "capability").
    Kernel(KernelClass),
    /// Refers to a policy-defined class by its policy-defined numeric Id. This is most commonly
    /// used when handling queries from userspace, which refer to classes by-Id.
    ClassId(ClassId),
}

impl From<ClassId> for ObjectClass {
    fn from(id: ClassId) -> Self {
        Self::ClassId(id)
    }
}

impl<T: Into<KernelClass>> From<T> for ObjectClass {
    fn from(class: T) -> Self {
        Self::Kernel(class.into())
    }
}

/// Declares an `enum` and implements an `all_variants()` API for it.
macro_rules! enumerable_enum {
    ($(#[$meta:meta])* $name:ident $(extends $common_name:ident)? {
        $($(#[$variant_meta:meta])* $variant:ident,)*
    }) => {
        $(#[$meta])*
        pub enum $name {
            $($(#[$variant_meta])* $variant,)*
            $(Common($common_name),)?
        }

        impl $name {
            pub fn all_variants() -> Vec<Self> {
                let all_variants = vec![$($name::$variant),*];
                $(let mut all_variants = all_variants; all_variants.extend($common_name::all_variants().into_iter().map(Self::Common));)?
                all_variants
            }
        }
    }
}

enumerable_enum! {
    /// A well-known class in SELinux policy that has a particular meaning in policy enforcement
    /// hooks.
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    KernelClass {
        // keep-sorted start
        /// The SELinux "anon_inode" object class.
        AnonFsNode,
        /// The SELinux "blk_file" object class.
        Block,
        /// The SELinux "bpf" object class.
        Bpf,
        /// The SELinux "capability" object class.
        Capability,
        /// The SELinux "capability2" object class.
        Capability2,
        /// The SELinux "chr_file" object class.
        Character,
        /// The SELinux "dir" object class.
        Dir,
        /// The SELinux "fd" object class.
        Fd,
        /// The SELinux "fifo_file" object class.
        Fifo,
        /// The SELinux "file" object class.
        File,
        /// The SELinux "filesystem" object class.
        FileSystem,
        /// The SELinux "key_socket" object class.
        KeySocket,
        /// The SELinux "lnk_file" object class.
        Link,
        /// The SELinux "netlink_audit_socket" object class.
        NetlinkAuditSocket,
        /// The SELinux "netlink_connector_socket" object class.
        NetlinkConnectorSocket,
        /// The SELinux "netlink_crypto_socket" object class.
        NetlinkCryptoSocket,
        /// The SELinux "netlink_dnrt_socket" object class.
        NetlinkDnrtSocket,
        /// The SELinux "netlink_fib_lookup_socket" object class.
        NetlinkFibLookupSocket,
        /// The SELinux "netlink_firewall_socket" object class.
        NetlinkFirewallSocket,
        /// The SELinux "netlink_generic_socket" object class.
        NetlinkGenericSocket,
        /// The SELinux "netlink_ip6fw_socket" object class.
        NetlinkIp6FwSocket,
        /// The SELinux "netlink_iscsi_socket" object class.
        NetlinkIscsiSocket,
        /// The SELinux "netlink_kobject_uevent_socket" object class.
        NetlinkKobjectUeventSocket,
        /// The SELinux "netlink_netfilter_socket" object class.
        NetlinkNetfilterSocket,
        /// The SELinux "netlink_nflog_socket" object class.
        NetlinkNflogSocket,
        /// The SELinux "netlink_rdma_socket" object class.
        NetlinkRdmaSocket,
        /// The SELinux "netlink_route_socket" object class.
        NetlinkRouteSocket,
        /// The SELinux "netlink_scsitransport_socket" object class.
        NetlinkScsitransportSocket,
        /// The SELinux "netlink_selinux_socket" object class.
        NetlinkSelinuxSocket,
        /// The SELinux "netlink_socket" object class.
        NetlinkSocket,
        /// The SELinux "netlink_tcpdiag_socket" object class.
        NetlinkTcpDiagSocket,
        /// The SELinux "netlink_xfrm_socket" object class.
        NetlinkXfrmSocket,
        /// The SELinux "packet_socket" object class.
        PacketSocket,
        /// The SELinux "process" object class.
        Process,
        /// The SELinux "rawip_socket" object class.
        RawIpSocket,
        /// The SELinux "security" object class.
        Security,
        /// The SELinux "sock_file" object class.
        SockFile,
        /// The SELinux "socket" object class.
        Socket,
        /// The SELinux "tcp_socket" object class.
        TcpSocket,
        /// The SELinux "udp_socket" object class.
        UdpSocket,
        /// The SELinux "unix_dgram_socket" object class.
        UnixDgramSocket,
        /// The SELinux "unix_stream_socket" object class.
        UnixStreamSocket,
        /// The SELinux "vsock_socket" object class.
        VSockSocket,
        // keep-sorted end
    }
}

impl KernelClass {
    /// Returns the name used to refer to this object class in the SELinux binary policy.
    pub fn name(&self) -> &'static str {
        match self {
            // keep-sorted start
            Self::AnonFsNode => "anon_inode",
            Self::Block => "blk_file",
            Self::Bpf => "bpf",
            Self::Capability => "capability",
            Self::Capability2 => "capability2",
            Self::Character => "chr_file",
            Self::Dir => "dir",
            Self::Fd => "fd",
            Self::Fifo => "fifo_file",
            Self::File => "file",
            Self::FileSystem => "filesystem",
            Self::KeySocket => "key_socket",
            Self::Link => "lnk_file",
            Self::NetlinkAuditSocket => "netlink_audit_socket",
            Self::NetlinkConnectorSocket => "netlink_connector_socket",
            Self::NetlinkCryptoSocket => "netlink_crypto_socket",
            Self::NetlinkDnrtSocket => "netlink_dnrt_socket",
            Self::NetlinkFibLookupSocket => "netlink_fib_lookup_socket",
            Self::NetlinkFirewallSocket => "netlink_firewall_socket",
            Self::NetlinkGenericSocket => "netlink_generic_socket",
            Self::NetlinkIp6FwSocket => "netlink_ip6fw_socket",
            Self::NetlinkIscsiSocket => "netlink_iscsi_socket",
            Self::NetlinkKobjectUeventSocket => "netlink_kobject_uevent_socket",
            Self::NetlinkNetfilterSocket => "netlink_netfilter_socket",
            Self::NetlinkNflogSocket => "netlink_nflog_socket",
            Self::NetlinkRdmaSocket => "netlink_rdma_socket",
            Self::NetlinkRouteSocket => "netlink_route_socket",
            Self::NetlinkScsitransportSocket => "netlink_scsitransport_socket",
            Self::NetlinkSelinuxSocket => "netlink_selinux_socket",
            Self::NetlinkSocket => "netlink_socket",
            Self::NetlinkTcpDiagSocket => "netlink_tcpdiag_socket",
            Self::NetlinkXfrmSocket => "netlink_xfrm_socket",
            Self::PacketSocket => "packet_socket",
            Self::Process => "process",
            Self::RawIpSocket => "rawip_socket",
            Self::Security => "security",
            Self::SockFile => "sock_file",
            Self::Socket => "socket",
            Self::TcpSocket => "tcp_socket",
            Self::UdpSocket => "udp_socket",
            Self::UnixDgramSocket => "unix_dgram_socket",
            Self::UnixStreamSocket => "unix_stream_socket",
            Self::VSockSocket => "vsock_socket",
            // keep-sorted end
        }
    }
}

enumerable_enum! {
    /// Covers the set of classes that inherit from the common "cap" symbol (e.g. "capability" for
    /// now and "cap_userns" after Starnix gains user namespacing support).
    #[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
    CapClass {
        // keep-sorted start
        /// The SELinux "capability" object class.
        Capability,
        // keep-sorted end
    }
}

impl From<CapClass> for KernelClass {
    fn from(cap_class: CapClass) -> Self {
        match cap_class {
            // keep-sorted start
            CapClass::Capability => Self::Capability,
            // keep-sorted end
        }
    }
}

enumerable_enum! {
    /// Covers the set of classes that inherit from the common "cap2" symbol (e.g. "capability2" for
    /// now and "cap2_userns" after Starnix gains user namespacing support).
    #[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
    Cap2Class {
        // keep-sorted start
        /// The SELinux "capability2" object class.
        Capability2,
        // keep-sorted end
    }
}

impl From<Cap2Class> for KernelClass {
    fn from(cap2_class: Cap2Class) -> Self {
        match cap2_class {
            // keep-sorted start
            Cap2Class::Capability2 => Self::Capability2,
            // keep-sorted end
        }
    }
}

enumerable_enum! {
    /// A well-known file-like class in SELinux policy that has a particular meaning in policy
    /// enforcement hooks.
    #[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
    FileClass {
        // keep-sorted start
        /// The SELinux "anon_inode" object class.
        AnonFsNode,
        /// The SELinux "blk_file" object class.
        Block,
        /// The SELinux "chr_file" object class.
        Character,
        /// The SELinux "dir" object class.
        Dir,
        /// The SELinux "fifo_file" object class.
        Fifo,
        /// The SELinux "file" object class.
        File,
        /// The SELinux "lnk_file" object class.
        Link,
        /// The SELinux "sock_file" object class.
        SockFile,
        // keep-sorted end
    }
}

impl From<FileClass> for KernelClass {
    fn from(file_class: FileClass) -> Self {
        match file_class {
            // keep-sorted start
            FileClass::AnonFsNode => Self::AnonFsNode,
            FileClass::Block => Self::Block,
            FileClass::Character => Self::Character,
            FileClass::Dir => Self::Dir,
            FileClass::Fifo => Self::Fifo,
            FileClass::File => Self::File,
            FileClass::Link => Self::Link,
            FileClass::SockFile => Self::SockFile,
            // keep-sorted end
        }
    }
}

enumerable_enum! {
    /// Distinguishes socket-like kernel object classes defined in SELinux policy.
    #[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
    SocketClass {
        // keep-sorted start
        Key,
        Netlink,
        NetlinkAudit,
        NetlinkConnector,
        NetlinkCrypto,
        NetlinkDnrt,
        NetlinkFibLookup,
        NetlinkFirewall,
        NetlinkGeneric,
        NetlinkIp6Fw,
        NetlinkIscsi,
        NetlinkKobjectUevent,
        NetlinkNetfilter,
        NetlinkNflog,
        NetlinkRdma,
        NetlinkRoute,
        NetlinkScsitransport,
        NetlinkSelinux,
        NetlinkTcpDiag,
        NetlinkXfrm,
        Packet,
        RawIp,
        /// Generic socket class applied to all socket-like objects for which no more specific
        /// class is defined.
        Socket,
        Tcp,
        Udp,
        UnixDgram,
        UnixStream,
        Vsock,
        // keep-sorted end
    }
}

impl From<SocketClass> for KernelClass {
    fn from(socket_class: SocketClass) -> Self {
        match socket_class {
            // keep-sorted start
            SocketClass::Key => Self::KeySocket,
            SocketClass::Netlink => Self::NetlinkSocket,
            SocketClass::NetlinkAudit => Self::NetlinkAuditSocket,
            SocketClass::NetlinkConnector => Self::NetlinkConnectorSocket,
            SocketClass::NetlinkCrypto => Self::NetlinkCryptoSocket,
            SocketClass::NetlinkDnrt => Self::NetlinkDnrtSocket,
            SocketClass::NetlinkFibLookup => Self::NetlinkFibLookupSocket,
            SocketClass::NetlinkFirewall => Self::NetlinkFirewallSocket,
            SocketClass::NetlinkGeneric => Self::NetlinkGenericSocket,
            SocketClass::NetlinkIp6Fw => Self::NetlinkIp6FwSocket,
            SocketClass::NetlinkIscsi => Self::NetlinkIscsiSocket,
            SocketClass::NetlinkKobjectUevent => Self::NetlinkDnrtSocket,
            SocketClass::NetlinkNetfilter => Self::NetlinkNetfilterSocket,
            SocketClass::NetlinkNflog => Self::NetlinkNflogSocket,
            SocketClass::NetlinkRdma => Self::NetlinkRdmaSocket,
            SocketClass::NetlinkRoute => Self::NetlinkRouteSocket,
            SocketClass::NetlinkScsitransport => Self::NetlinkScsitransportSocket,
            SocketClass::NetlinkSelinux => Self::NetlinkSelinuxSocket,
            SocketClass::NetlinkTcpDiag => Self::NetlinkTcpDiagSocket,
            SocketClass::NetlinkXfrm => Self::NetlinkXfrmSocket,
            SocketClass::Packet => Self::PacketSocket,
            SocketClass::RawIp => Self::RawIpSocket,
            SocketClass::Socket => Self::Socket,
            SocketClass::Tcp => Self::TcpSocket,
            SocketClass::Udp => Self::UdpSocket,
            SocketClass::UnixDgram => Self::UnixDgramSocket,
            SocketClass::UnixStream => Self::UnixStreamSocket,
            SocketClass::Vsock => Self::VSockSocket,
            // keep-sorted end
        }
    }
}

/// Container for a security class that could be associated with a [`crate::vfs::FsNode`], to allow
/// permissions common to both file-like and socket-like classes to be generated easily by hooks.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum FsNodeClass {
    File(FileClass),
    Socket(SocketClass),
}

impl From<FsNodeClass> for KernelClass {
    fn from(class: FsNodeClass) -> Self {
        match class {
            FsNodeClass::File(file_class) => file_class.into(),
            FsNodeClass::Socket(sock_class) => sock_class.into(),
        }
    }
}

impl From<FileClass> for FsNodeClass {
    fn from(file_class: FileClass) -> Self {
        FsNodeClass::File(file_class)
    }
}

impl From<SocketClass> for FsNodeClass {
    fn from(sock_class: SocketClass) -> Self {
        FsNodeClass::Socket(sock_class)
    }
}

pub trait ClassPermission {
    fn class(&self) -> KernelClass;
}

macro_rules! permission_enum {
    ($(#[$meta:meta])* $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident($inner:ident)),*,
    }) => {
        $(#[$meta])*
        pub enum $name {
            $($(#[$variant_meta])* $variant($inner)),*
        }

        $(impl From<$inner> for $name {
            fn from(v: $inner) -> Self {
                Self::$variant(v)
            }
        })*

        impl ClassPermission for $name {
            fn class(&self) -> KernelClass {
                match self {
                    $($name::$variant(_) => KernelClass::$variant),*
                }
            }
        }

        impl $name {
            pub fn name(&self) -> &'static str {
                match self {
                    $($name::$variant(v) => v.name()),*
                }
            }

            pub fn all_variants() -> Vec<Self> {
                let mut all_variants = vec![];
                $(all_variants.extend($inner::all_variants().into_iter().map($name::from));)*
                all_variants
            }
        }
    }
}

permission_enum! {
    /// A well-known `(class, permission)` pair in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    KernelPermission {
        // keep-sorted start
        /// Permissions for the well-known SELinux "anon_inode" file-like object class.
        AnonFsNode(AnonFsNodePermission),
        /// Permissions for the well-known SELinux "blk_file" file-like object class.
        Block(BlockFilePermission),
        /// Permissions for the well-known SELinux "bpf" file-like object class.
        Bpf(BpfPermission),
        /// Permissions for the well-known SELinux "capability" object class.
        Capability(CapabilityPermission),
        /// Permissions for the well-known SELinux "capability2" object class.
        Capability2(Capability2Permission),
        /// Permissions for the well-known SELinux "chr_file" file-like object class.
        Character(CharacterFilePermission),
        /// Permissions for the well-known SELinux "dir" file-like object class.
        Dir(DirPermission),
        /// Permissions for the well-known SELinux "fd" object class.
        Fd(FdPermission),
        /// Permissions for the well-known SELinux "fifo_file" file-like object class.
        Fifo(FifoFilePermission),
        /// Permissions for the well-known SELinux "file" object class.
        File(FilePermission),
        /// Permissions for the well-known SELinux "filesystem" object class.
        FileSystem(FileSystemPermission),
        /// Permissions for the well-known SELinux "packet_socket" object class.
        KeySocket(KeySocketPermission),
        /// Permissions for the well-known SELinux "lnk_file" file-like object class.
        Link(LinkFilePermission),
        /// Permissions for the well-known SELinux "netlink_audit_socket" file-like object class.
        NetlinkAuditSocket(NetlinkAuditSocketPermission),
        /// Permissions for the well-known SELinux "netlink_connector_socket" file-like object class.
        NetlinkConnectorSocket(NetlinkConnectorSocketPermission),
        /// Permissions for the well-known SELinux "netlink_crypto_socket" file-like object class.
        NetlinkCryptoSocket(NetlinkCryptoSocketPermission),
        /// Permissions for the well-known SELinux "netlink_dnrt_socket" file-like object class.
        NetlinkDnrtSocket(NetlinkDnrtSocketPermission),
        /// Permissions for the well-known SELinux "netlink_fib_lookup_socket" file-like object class.
        NetlinkFibLookupSocket(NetlinkFibLookupSocketPermission),
        /// Permissions for the well-known SELinux "netlink_firewall_socket" file-like object class.
        NetlinkFirewallSocket(NetlinkFirewallSocketPermission),
        /// Permissions for the well-known SELinux "netlink_generic_socket" file-like object class.
        NetlinkGenericSocket(NetlinkGenericSocketPermission),
        /// Permissions for the well-known SELinux "netlink_ip6fw_socket" file-like object class.
        NetlinkIp6FwSocket(NetlinkIp6FwSocketPermission),
        /// Permissions for the well-known SELinux "netlink_iscsi_socket" file-like object class.
        NetlinkIscsiSocket(NetlinkIscsiSocketPermission),
        /// Permissions for the well-known SELinux "netlink_kobject_uevent_socket" file-like object class.
        NetlinkKobjectUeventSocket(NetlinkKobjectUeventSocketPermission),
        /// Permissions for the well-known SELinux "netlink_netfilter_socket" file-like object class.
        NetlinkNetfilterSocket(NetlinkNetfilterSocketPermission),
        /// Permissions for the well-known SELinux "netlink_nflog_socket" file-like object class.
        NetlinkNflogSocket(NetlinkNflogSocketPermission),
        /// Permissions for the well-known SELinux "netlink_rdma_socket" file-like object class.
        NetlinkRdmaSocket(NetlinkRdmaSocketPermission),
        /// Permissions for the well-known SELinux "netlink_route_socket" file-like object class.
        NetlinkRouteSocket(NetlinkRouteSocketPermission),
        /// Permissions for the well-known SELinux "netlink_scsitransport_socket" file-like object class.
        NetlinkScsitransportSocket(NetlinkScsitransportSocketPermission),
        /// Permissions for the well-known SELinux "netlink_selinux_socket" file-like object class.
        NetlinkSelinuxSocket(NetlinkSelinuxSocketPermission),
        /// Permissions for the well-known SELinux "netlink_socket" file-like object class.
        NetlinkSocket(NetlinkSocketPermission),
        /// Permissions for the well-known SELinux "netlink_tcpdiag_socket" file-like object class.
        NetlinkTcpDiagSocket(NetlinkTcpDiagSocketPermission),
        /// Permissions for the well-known SELinux "netlink_xfrm_socket" file-like object class.
        NetlinkXfrmSocket(NetlinkXfrmSocketPermission),
        /// Permissions for the well-known SELinux "packet_socket" object class.
        PacketSocket(PacketSocketPermission),
        /// Permissions for the well-known SELinux "process" object class.
        Process(ProcessPermission),
        /// Permissions for the well-known SELinux "rawip_socket" object class.
        RawIpSocket(RawIpSocketPermission),
        /// Permissions for access to parts of the "selinuxfs" used to administer and query SELinux.
        Security(SecurityPermission),
        /// Permissions for the well-known SELinux "sock_file" file-like object class.
        SockFile(SockFilePermission),
        /// Permissions for the well-known SELinux "socket" object class.
        Socket(SocketPermission),
        /// Permissions for the well-known SELinux "tcp_socket" object class.
        TcpSocket(TcpSocketPermission),
        /// Permissions for the well-known SELinux "udp_socket" object class.
        UdpSocket(UdpSocketPermission),
        /// Permissions for the well-known SELinux "unix_dgram_socket" object class.
        UnixDgramSocket(UnixDgramSocketPermission),
        /// Permissions for the well-known SELinux "unix_stream_socket" object class.
        UnixStreamSocket(UnixStreamSocketPermission),
        /// Permissions for the well-known SELinux "vsock_socket" object class.
        VSockSocket(VsockSocketPermission),
        // keep-sorted end
    }
}

/// Helper used to define an enum of permission values, with specified names.
/// Uses of this macro should not rely on "extends", which is solely for use to express permission
/// inheritance in `class_permission_enum`.
macro_rules! common_permission_enum {
    ($(#[$meta:meta])* $name:ident $(extends $common_name:ident)? {
        $($(#[$variant_meta:meta])* $variant:ident ($variant_name:literal),)*
    }) => {
        enumerable_enum! {
            $(#[$meta])* $name $(extends $common_name)? {
                $($(#[$variant_meta])* $variant,)*
            }
        }

        impl $name {
            fn name(&self) -> &'static str {
                match self {
                    $($name::$variant => $variant_name,)*
                    $(Self::Common(v) => {let v:$common_name = v.clone(); v.name()},)?
                }
            }
        }
    }
}

/// Helper used to declare the set of named permissions associated with an SELinux class.
/// The `ClassType` trait is implemented on the declared `enum`, enabling values to be wrapped into
/// the generic `KernelPermission` container.
/// If an "extends" type is specified then a `Common` enum case is added, encapsulating the values
/// of that underlying permission type. This is used to represent e.g. SELinux "dir" class deriving
/// a basic set of permissions from the common "file" symbol.
macro_rules! class_permission_enum {
    ($(#[$meta:meta])* $name:ident $(extends $common_name:ident)? {
        $($(#[$variant_meta:meta])* $variant:ident ($variant_name:literal),)*
    }) => {
        common_permission_enum! {
            $(#[$meta])* $name $(extends $common_name)? {
                $($(#[$variant_meta])* $variant ($variant_name),)*
            }
        }

        impl ClassPermission for $name {
            fn class(&self) -> KernelClass {
                KernelPermission::from(self.clone()).class()
            }
        }
    }
}

common_permission_enum! {
    /// Permissions common to all cap-like object classes (e.g. "capability" for now and
    /// "cap_userns" after Starnix gains user namespacing support). These are combined with a
    /// specific `CapabilityClass` by policy enforcement hooks, to obtain class-affine permission
    /// values to check.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    CommonCapPermission {
        // keep-sorted start

        AuditControl("audit_control"),
        AuditWrite("audit_write"),
        Chown("chown"),
        DacOverride("dac_override"),
        DacReadSearch("dac_read_search"),
        Fowner("fowner"),
        Fsetid("fsetid"),
        IpcLock("ipc_lock"),
        IpcOwner("ipc_owner"),
        Kill("kill"),
        Lease("lease"),
        LinuxImmutable("linux_immutable"),
        Mknod("mknod"),
        NetAdmin("net_admin"),
        NetBindService("net_bind_service"),
        NetBroadcast("net_broadcast"),
        NetRaw("net_raw"),
        Setfcap("setfcap"),
        Setgid("setgid"),
        Setpcap("setpcap"),
        Setuid("setuid"),
        SysAdmin("sys_admin"),
        SysBoot("sys_boot"),
        SysChroot("sys_chroot"),
        SysModule("sys_module"),
        SysNice("sys_nice"),
        SysPacct("sys_pacct"),
        SysPtrace("sys_ptrace"),
        SysRawio("sys_rawio"),
        SysResource("sys_resource"),
        SysTime("sys_time"),
        SysTtyConfig("sys_tty_config"),

        // keep-sorted end
    }
}

class_permission_enum! {
    /// A well-known "capability" class permission in SELinux policy that has a particular meaning
    /// in policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    CapabilityPermission extends CommonCapPermission {}
}

impl CommonCapPermission {
    /// Returns the `class`-affine `KernelPermission` value corresponding to this common permission.
    /// This is used to allow hooks to resolve e.g. common "sys_nice" permission access based on the
    /// "allow" rules for the correct target object class.
    pub fn for_class(&self, class: CapClass) -> KernelPermission {
        match class {
            CapClass::Capability => CapabilityPermission::Common(self.clone()).into(),
        }
    }
}

common_permission_enum! {
    /// Permissions common to all cap2-like object classes (e.g. "capability2" for now and
    /// "cap2_userns" after Starnix gains user namespacing support). These are combined with a
    /// specific `Capability2Class` by policy enforcement hooks, to obtain class-affine permission
    /// values to check.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    CommonCap2Permission {
        // keep-sorted start

        AuditRead("audit_read"),
        BlockSuspend("block_suspend"),
        Bpf("bpf"),
        MacAdmin("mac_admin"),
        MacOverride("mac_override"),
        Syslog("syslog"),
        WakeAlarm("wake_alarm"),

        // keep-sorted end
    }
}

class_permission_enum! {
    /// A well-known "capability2" class permission in SELinux policy that has a particular meaning
    /// in policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    Capability2Permission extends CommonCap2Permission {}
}

impl CommonCap2Permission {
    /// Returns the `class`-affine `KernelPermission` value corresponding to this common permission.
    /// This is used to allow hooks to resolve e.g. common "mac_admin" permission access based on
    /// the "allow" rules for the correct target object class.
    pub fn for_class(&self, class: Cap2Class) -> KernelPermission {
        match class {
            Cap2Class::Capability2 => Capability2Permission::Common(self.clone()).into(),
        }
    }
}

common_permission_enum! {
    /// Permissions meaningful for all [`crate::vfs::FsNode`]s, whether file- or socket-like.
    ///
    /// This extra layer of common permissions is not reflected in the hierarchy defined by the
    /// SELinux Reference Policy. Because even common permissions are mapped per-class, by name, to
    /// the policy equivalents, the implementation and policy notions of common permissions need not
    /// be identical.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    CommonFsNodePermission {
        // keep-sorted start
        /// Permission to append to a file or socket.
        Append("append"),
        /// Permission to create a file or socket.
        Create("create"),
        /// Permission to query attributes, including uid, gid and extended attributes.
        GetAttr("getattr"),
        /// Permission to execute ioctls on the file or socket.
        Ioctl("ioctl"),
        /// Permission to set and unset file or socket locks.
        Lock("lock"),
        /// Permission to map a file.
        Map("map"),
        /// Permission to read content from a file or socket, as well as reading or following links.
        Read("read"),
        /// Permission checked against the existing label when updating a node's security label.
        RelabelFrom("relabelfrom"),
        /// Permission checked against the new label when updating a node's security label.
        RelabelTo("relabelto"),
        /// Permission to modify attributes, including uid, gid and extended attributes.
        SetAttr("setattr"),
        /// Permission to write contents to the file or socket.
        Write("write"),
        // keep-sorted end
    }
}

impl CommonFsNodePermission {
    /// Returns the `class`-affine `KernelPermission` value corresponding to this common permission.
    /// This is used to allow hooks to resolve e.g. common "read" permission access based on the
    /// "allow" rules for the correct target object class.
    pub fn for_class(&self, class: impl Into<FsNodeClass>) -> KernelPermission {
        match class.into() {
            FsNodeClass::File(file_class) => {
                CommonFilePermission::Common(self.clone()).for_class(file_class)
            }
            FsNodeClass::Socket(sock_class) => {
                CommonSocketPermission::Common(self.clone()).for_class(sock_class)
            }
        }
    }
}
common_permission_enum! {
    /// Permissions common to all socket-like object classes. These are combined with a specific
    /// `SocketClass` by policy enforcement hooks, to obtain class-affine permission values.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    CommonSocketPermission extends CommonFsNodePermission {
        // keep-sorted start
        /// Permission to bind to a name.
        Bind("bind"),
        /// Permission to initiate a connection.
        Connect("connect"),
        /// Permission to listen for connections.
        Listen("listen"),
        /// Permission to send datagrams to the socket.
        SendTo("sendto"),
        /// Permission to terminate connection.
        Shutdown("shutdown"),
        // keep-sorted end
    }
}

impl CommonSocketPermission {
    /// Returns the `class`-affine `KernelPermission` value corresponding to this common permission.
    /// This is used to allow hooks to resolve e.g. common "read" permission access based on the
    /// "allow" rules for the correct target object class.
    pub fn for_class(&self, class: SocketClass) -> KernelPermission {
        match class {
            SocketClass::Key => KeySocketPermission::Common(self.clone()).into(),
            SocketClass::Netlink => NetlinkSocketPermission::Common(self.clone()).into(),
            SocketClass::NetlinkAudit => NetlinkAuditSocketPermission::Common(self.clone()).into(),
            SocketClass::NetlinkConnector => {
                NetlinkConnectorSocketPermission::Common(self.clone()).into()
            }
            SocketClass::NetlinkCrypto => {
                NetlinkCryptoSocketPermission::Common(self.clone()).into()
            }
            SocketClass::NetlinkDnrt => NetlinkDnrtSocketPermission::Common(self.clone()).into(),
            SocketClass::NetlinkFibLookup => {
                NetlinkFibLookupSocketPermission::Common(self.clone()).into()
            }
            SocketClass::NetlinkFirewall => {
                NetlinkFirewallSocketPermission::Common(self.clone()).into()
            }
            SocketClass::NetlinkGeneric => {
                NetlinkGenericSocketPermission::Common(self.clone()).into()
            }
            SocketClass::NetlinkIp6Fw => NetlinkIp6FwSocketPermission::Common(self.clone()).into(),
            SocketClass::NetlinkIscsi => NetlinkIscsiSocketPermission::Common(self.clone()).into(),
            SocketClass::NetlinkKobjectUevent => {
                NetlinkKobjectUeventSocketPermission::Common(self.clone()).into()
            }
            SocketClass::NetlinkNetfilter => {
                NetlinkNetfilterSocketPermission::Common(self.clone()).into()
            }
            SocketClass::NetlinkNflog => NetlinkNflogSocketPermission::Common(self.clone()).into(),
            SocketClass::NetlinkRdma => NetlinkRdmaSocketPermission::Common(self.clone()).into(),
            SocketClass::NetlinkRoute => NetlinkRouteSocketPermission::Common(self.clone()).into(),
            SocketClass::NetlinkScsitransport => {
                NetlinkScsitransportSocketPermission::Common(self.clone()).into()
            }
            SocketClass::NetlinkSelinux => {
                NetlinkSelinuxSocketPermission::Common(self.clone()).into()
            }
            SocketClass::NetlinkTcpDiag => {
                NetlinkTcpDiagSocketPermission::Common(self.clone()).into()
            }
            SocketClass::NetlinkXfrm => NetlinkXfrmSocketPermission::Common(self.clone()).into(),
            SocketClass::Packet => PacketSocketPermission::Common(self.clone()).into(),
            SocketClass::RawIp => RawIpSocketPermission::Common(self.clone()).into(),
            SocketClass::Socket => SocketPermission::Common(self.clone()).into(),
            SocketClass::Tcp => TcpSocketPermission::Common(self.clone()).into(),
            SocketClass::Udp => UdpSocketPermission::Common(self.clone()).into(),
            SocketClass::UnixDgram => UnixDgramSocketPermission::Common(self.clone()).into(),
            SocketClass::UnixStream => UnixStreamSocketPermission::Common(self.clone()).into(),
            SocketClass::Vsock => VsockSocketPermission::Common(self.clone()).into(),
        }
    }
}

class_permission_enum! {
    /// A well-known "key_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    KeySocketPermission extends CommonSocketPermission {
    }
}
class_permission_enum! {
    /// A well-known "netlink_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_route_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkRouteSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_firewall_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkFirewallSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_tcpdiag_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkTcpDiagSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_nflog_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkNflogSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_xfrm_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkXfrmSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_selinux_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkSelinuxSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_iscsi_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkIscsiSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_audit_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkAuditSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_fib_lookup_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkFibLookupSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_connector_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkConnectorSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_netfilter_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkNetfilterSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_ip6fw_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkIp6FwSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_dnrt_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkDnrtSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_kobject_uevent_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkKobjectUeventSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_generic_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkGenericSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_scsitransport_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkScsitransportSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_rdma_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkRdmaSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "netlink_crypto_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    NetlinkCryptoSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "packet_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    PacketSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "rawip_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    RawIpSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    SocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "tcp_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    TcpSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "udp_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    UdpSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "unix_stream_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    UnixStreamSocketPermission extends CommonSocketPermission {
        // keep-sorted start
        /// Permission to connect a streaming Unix-domain socket.
        ConnectTo("connectto"),
        // keep-sorted end
    }
}

class_permission_enum! {
    /// A well-known "unix_dgram_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    UnixDgramSocketPermission extends CommonSocketPermission {
    }
}

class_permission_enum! {
    /// A well-known "vsock_socket" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    VsockSocketPermission extends CommonSocketPermission {
    }
}

common_permission_enum! {
    /// Permissions common to all file-like object classes (e.g. "lnk_file", "dir"). These are
    /// combined with a specific `FileClass` by policy enforcement hooks, to obtain class-affine
    /// permission values to check.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    CommonFilePermission extends CommonFsNodePermission {
        // keep-sorted start
        /// Permission to execute a file with domain transition.
        Execute("execute"),
        /// Permissions to create hard link.
        Link("link"),
        /// Permission to use as mount point; only useful for directories and files.
        MountOn("mounton"),
        /// Permission to open a file.
        Open("open"),
        /// Permission to rename a file.
        Rename("rename"),
        /// Permission to delete a file or remove a hard link.
        Unlink("unlink"),
        // keep-sorted end
    }
}

impl CommonFilePermission {
    /// Returns the `class`-affine `KernelPermission` value corresponding to this common permission.
    /// This is used to allow hooks to resolve e.g. common "read" permission access based on the
    /// "allow" rules for the correct target object class.
    pub fn for_class(&self, class: FileClass) -> KernelPermission {
        match class {
            FileClass::AnonFsNode => AnonFsNodePermission::Common(self.clone()).into(),
            FileClass::Block => BlockFilePermission::Common(self.clone()).into(),
            FileClass::Character => CharacterFilePermission::Common(self.clone()).into(),
            FileClass::Dir => DirPermission::Common(self.clone()).into(),
            FileClass::Fifo => FifoFilePermission::Common(self.clone()).into(),
            FileClass::File => FilePermission::Common(self.clone()).into(),
            FileClass::Link => LinkFilePermission::Common(self.clone()).into(),
            FileClass::SockFile => SockFilePermission::Common(self.clone()).into(),
        }
    }
}

class_permission_enum! {
    /// A well-known "anon_file" class permission used to manage special file-like nodes not linked
    /// into any directory structures.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    AnonFsNodePermission extends CommonFilePermission {
    }
}

class_permission_enum! {
    /// A well-known "blk_file" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    BlockFilePermission extends CommonFilePermission {
    }
}

class_permission_enum! {
    /// A well-known "chr_file" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    CharacterFilePermission extends CommonFilePermission {
    }
}

class_permission_enum! {
    /// A well-known "dir" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    DirPermission extends CommonFilePermission {
        // keep-sorted start
        /// Permission to add a file to the directory.
        AddName("add_name"),
        /// Permission to remove a directory.
        RemoveDir("rmdir"),
        /// Permission to remove an entry from a directory.
        RemoveName("remove_name"),
        /// Permission to change parent directory.
        Reparent("reparent"),
        /// Search access to the directory.
        Search("search"),
        // keep-sorted end
    }
}

class_permission_enum! {
    /// A well-known "fd" class permission in SELinux policy that has a particular meaning in policy
    /// enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    FdPermission {
        // keep-sorted start
        /// Permission to use file descriptors copied/retained/inherited from another security
        /// context. This permission is generally used to control whether an `exec*()` call from a
        /// cloned process that retained a copy of the file descriptor table should succeed.
        Use("use"),
        // keep-sorted end
    }
}

class_permission_enum! {
    /// A well-known "bpf" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    BpfPermission {
        // keep-sorted start
        /// Permission to create a map.
        MapCreate("map_create"),
        /// Permission to read from a map.
        MapRead("map_read"),
        /// Permission to write on a map.
        MapWrite("map_write"),
        /// Permission to load a program.
        ProgLoad("prog_load"),
        /// Permission to run a program.
        ProgRun("prog_run"),
        // keep-sorted end
    }
}

class_permission_enum! {
    /// A well-known "fifo_file" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    FifoFilePermission extends CommonFilePermission {
    }
}

class_permission_enum! {
    /// A well-known "file" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    FilePermission extends CommonFilePermission {
        // keep-sorted start
        /// Permission to use a file as an entry point into the new domain on transition.
        Entrypoint("entrypoint"),
        /// Permission to use a file as an entry point to the calling domain without performing a
        /// transition.
        ExecuteNoTrans("execute_no_trans"),
        // keep-sorted end
    }
}

class_permission_enum! {
    /// A well-known "filesystem" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    FileSystemPermission {
        // keep-sorted start
        /// Permission to associate a file to the filesystem.
        Associate("associate"),
        /// Permission to get filesystem attributes.
        GetAttr("getattr"),
        /// Permission mount a filesystem.
        Mount("mount"),
        /// Permission to remount a filesystem with different flags.
        Remount("remount"),
        /// Permission to unmount a filesystem.
        Unmount("unmount"),
        // keep-sorted end
    }
}

class_permission_enum! {
    /// A well-known "lnk_file" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    LinkFilePermission extends CommonFilePermission {
    }
}

class_permission_enum! {
    /// A well-known "sock_file" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    SockFilePermission extends CommonFilePermission {
    }
}

class_permission_enum! {
    /// A well-known "process" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    ProcessPermission {
        // keep-sorted start
        /// Permission to dynamically transition a process to a different security domain.
        DynTransition("dyntransition"),
        /// Permission to execute arbitrary code from memory.
        ExecMem("execmem"),
        /// Permission to fork the current running process.
        Fork("fork"),
        /// Permission to get the process group ID.
        GetPgid("getpgid"),
        /// Permission to get the resource limits on a process.
        GetRlimit("getrlimit"),
        /// Permission to get scheduling policy currently applied to a process.
        GetSched("getsched"),
        /// Permission to get the session ID.
        GetSession("getsession"),
        /// Permission to trace a process.
        Ptrace("ptrace"),
        /// Permission to inherit the parent process's resource limits on exec.
        RlimitInh("rlimitinh"),
        /// Permission to set the calling task's current Security Context.
        /// The "dyntransition" permission separately limits which Contexts "setcurrent" may be used to transition to.
        SetCurrent("setcurrent"),
        /// Permission to set the Security Context used by `exec()`.
        SetExec("setexec"),
        /// Permission to set the Security Context used when creating filesystem objects.
        SetFsCreate("setfscreate"),
        /// Permission to set the Security Context used when creating kernel keyrings.
        SetKeyCreate("setkeycreate"),
        /// Permission to set the process group ID.
        SetPgid("setpgid"),
        /// Permission to set the resource limits on a process.
        SetRlimit("setrlimit"),
        /// Permission to set scheduling policy for a process.
        SetSched("setsched"),
        /// Permission to set the Security Context used when creating new labeled sockets.
        SetSockCreate("setsockcreate"),
        /// Permission to share resources (e.g. FD table, address-space, etc) with a process.
        Share("share"),
        /// Permission to send SIGCHLD to a process.
        SigChld("sigchld"),
        /// Permission to send SIGKILL to a process.
        SigKill("sigkill"),
        /// Permission to send SIGSTOP to a process.
        SigStop("sigstop"),
        /// Permission to send a signal other than SIGKILL, SIGSTOP, or SIGCHLD to a process.
        Signal("signal"),
        /// Permission to transition to a different security domain.
        Transition("transition"),
        // keep-sorted end
    }
}

class_permission_enum! {
    /// A well-known "security" class permission in SELinux policy, used to control access to
    /// sensitive administrative and query API surfaces in the "selinuxfs".
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    SecurityPermission {
        // keep-sorted start
        /// Permission to validate Security Context using the "context" API.
        CheckContext("check_context"),
        /// Permission to compute access vectors via the "access" API.
        ComputeAv("compute_av"),
        /// Permission to compute security contexts based on `type_transition` rules via "create".
        ComputeCreate("compute_create"),
        /// Permission to compute security contexts based on `type_member` rules via "member".
        ComputeMember("compute_member"),
        /// Permission to compute security contexts based on `type_change` rules via "relabel".
        ComputeRelabel("compute_relabel"),
        /// Permission to compute user decisions via "user".
        ComputeUser("compute_user"),
        /// Permission to load a new binary policy into the kernel via the "load" API.
        LoadPolicy("load_policy"),
        /// Permission to commit booleans to control conditional elements of the policy.
        SetBool("setbool"),
        /// Permission to change the way permissions are validated for `mmap()` operations.
        SetCheckReqProt("setcheckreqprot"),
        /// Permission to switch the system between permissive and enforcing modes, via "enforce".
        SetEnforce("setenforce"),
        // keep-sorted end
     }
}

/// Initial Security Identifier (SID) values defined by the SELinux Reference Policy.
/// Where the SELinux Reference Policy retains definitions for some deprecated initial SIDs, this
/// enum omits deprecated entries for clarity.
#[repr(u64)]
enum ReferenceInitialSid {
    Kernel = 1,
    Security = 2,
    Unlabeled = 3,
    _Fs = 4,
    File = 5,
    _AnySocket = 6,
    _Port = 7,
    _Netif = 8,
    _Netmsg = 9,
    _Node = 10,
    _Sysctl = 15,
    _Devnull = 25,

    FirstUnused,
}

/// Lowest Security Identifier value guaranteed not to be used by this
/// implementation to refer to an initial Security Context.
pub const FIRST_UNUSED_SID: u32 = ReferenceInitialSid::FirstUnused as u32;

macro_rules! initial_sid_enum {
    ($(#[$meta:meta])* $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident ($variant_name: literal)),*,
    }) => {
        $(#[$meta])*
        pub enum $name {
            $($(#[$variant_meta])* $variant = ReferenceInitialSid::$variant as isize),*
        }

        impl $name {
            pub fn all_variants() -> Vec<Self> {
                vec![
                    $($name::$variant),*
                ]
            }

            pub fn name(&self) -> &'static str {
                match self {
                    $($name::$variant => $variant_name),*
                }
            }
        }
    }
}

initial_sid_enum! {
/// Initial Security Identifier (SID) values actually used by this implementation.
/// These must be present in the policy, for it to be valid.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
    InitialSid {
        // keep-sorted start
        File("file"),
        Kernel("kernel"),
        Security("security"),
        Unlabeled("unlabeled"),
        // keep-sorted end
    }
}

/// A borrowed byte slice that contains no `NUL` characters by truncating the input slice at the
/// first `NUL` (if any) upon construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NullessByteStr<'a>(&'a [u8]);

impl<'a> NullessByteStr<'a> {
    /// Returns a non-null-terminated representation of the security context string.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl<'a, S: AsRef<[u8]> + ?Sized> From<&'a S> for NullessByteStr<'a> {
    /// Any `AsRef<[u8]>` can be processed into a [`NullessByteStr`]. The [`NullessByteStr`] will
    /// retain everything up to (but not including) a null character, or else the complete byte
    /// string.
    fn from(s: &'a S) -> Self {
        let value = s.as_ref();
        match value.iter().position(|c| *c == 0) {
            Some(end) => Self(&value[..end]),
            None => Self(value),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileSystemMountSids {
    pub context: Option<SecurityId>,
    pub fs_context: Option<SecurityId>,
    pub def_context: Option<SecurityId>,
    pub root_context: Option<SecurityId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileSystemLabel {
    pub sid: SecurityId,
    pub scheme: FileSystemLabelingScheme,
    // Sids obtained by parsing the mount options of the FileSystem.
    pub mount_sids: FileSystemMountSids,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FileSystemLabelingScheme {
    /// This filesystem was mounted with "context=".
    Mountpoint { sid: SecurityId },
    /// This filesystem has an "fs_use_xattr", "fs_use_task", or "fs_use_trans" entry in the
    /// policy. `root_sid` identifies the context for the root of the filesystem and
    /// `computed_def_sid`  identifies the context to use for unlabeled files in the filesystem
    /// (the "default context").
    FsUse { fs_use_type: FsUseType, computed_def_sid: SecurityId },
    /// This filesystem has one or more "genfscon" statements associated with it in the policy.
    GenFsCon,
}

/// SELinux security context-related filesystem mount options. These options are documented in the
/// `context=context, fscontext=context, defcontext=context, and rootcontext=context` section of
/// the `mount(8)` manpage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileSystemMountOptions {
    /// Specifies the effective security context to use for all nodes in the filesystem, and the
    /// filesystem itself. If the filesystem already contains security attributes then these are
    /// ignored. May not be combined with any of the other options.
    pub context: Option<Vec<u8>>,
    /// Specifies an effective security context to use for un-labeled nodes in the filesystem,
    /// rather than falling-back to the policy-defined "file" context.
    pub def_context: Option<Vec<u8>>,
    /// The value of the `fscontext=[security-context]` mount option. This option is used to
    /// label the filesystem (superblock) itself.
    pub fs_context: Option<Vec<u8>>,
    /// The value of the `rootcontext=[security-context]` mount option. This option is used to
    /// (re)label the inode located at the filesystem mountpoint.
    pub root_context: Option<Vec<u8>>,
}

/// Status information parameter for the [`SeLinuxStatusPublisher`] interface.
pub struct SeLinuxStatus {
    /// SELinux-wide enforcing vs. permissive mode  bit.
    pub is_enforcing: bool,
    /// Number of times the policy has been changed since SELinux started.
    pub change_count: u32,
    /// Bit indicating whether operations unknown SELinux abstractions will be denied.
    pub deny_unknown: bool,
}

/// Interface for security server to interact with selinuxfs status file.
pub trait SeLinuxStatusPublisher: Send {
    /// Sets the value part of the associated selinuxfs status file.
    fn set_status(&mut self, policy_status: SeLinuxStatus);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_class_permissions() {
        let test_class_id = ClassId::new(NonZeroU32::new(20).unwrap());
        assert_eq!(ObjectClass::ClassId(test_class_id), test_class_id.into());
        for variant in ProcessPermission::all_variants().into_iter() {
            assert_eq!(KernelClass::Process, variant.class());
            assert_eq!("process", variant.class().name());
            let permission: KernelPermission = variant.clone().into();
            assert_eq!(KernelPermission::Process(variant.clone()), permission);
            assert_eq!(ObjectClass::Kernel(KernelClass::Process), variant.class().into());
        }
    }

    #[test]
    fn nulless_byte_str_equivalence() {
        let unterminated: NullessByteStr<'_> = b"u:object_r:test_valid_t:s0".into();
        let nul_terminated: NullessByteStr<'_> = b"u:object_r:test_valid_t:s0\0".into();
        let nul_containing: NullessByteStr<'_> =
            b"u:object_r:test_valid_t:s0\0IGNORE THIS\0!\0".into();

        for context in [nul_terminated, nul_containing] {
            assert_eq!(unterminated, context);
            assert_eq!(unterminated.as_bytes(), context.as_bytes());
        }
    }
}
