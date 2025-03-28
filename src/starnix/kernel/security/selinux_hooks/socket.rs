// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::audit::Auditable;
use crate::task::CurrentTask;
use crate::vfs::socket::{
    NetlinkFamily, Socket, SocketAddress, SocketDomain, SocketProtocol, SocketType,
};
use crate::vfs::FsNode;
use selinux::permission_check::PermissionCheck;
use selinux::{
    CommonSocketPermission, FsNodeClass, InitialSid, Permission, SecurityId, SecurityServer,
    SocketClass,
};
use starnix_uapi::errors::Errno;

use super::{check_permission, fs_node_effective_sid_and_class};

/// Checks that `current_task` has the specified `permission` for the `socket_node`.
fn has_socket_permission(
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    socket_node: &FsNode,
    permission: &Permission,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    let socket_sid = fs_node_effective_sid_and_class(socket_node).sid;

    // If the socket is for kernel-internal use we can return success immediately.
    // TODO(https://fxbug.dev/364569010): check if there are additional cases when the socket is for
    // kernel-internal use.
    if subject_sid == SecurityId::initial(InitialSid::Kernel)
        || socket_sid == SecurityId::initial(InitialSid::Kernel)
    {
        return Ok(());
    }
    let audit_context = [audit_context, socket_node.into()];
    check_permission(
        permission_check,
        subject_sid,
        socket_sid,
        permission.clone(),
        (&audit_context).into(),
    )
}

pub(in crate::security) fn socket_post_create(socket: &Socket, socket_node: &FsNode) {
    let security_class = match socket.domain {
        SocketDomain::Unix => match socket.socket_type {
            SocketType::Stream | SocketType::SeqPacket => SocketClass::UnixStream,
            SocketType::Datagram => SocketClass::UnixDgram,
            SocketType::Raw | SocketType::Rdm | SocketType::Dccp | SocketType::Packet => {
                SocketClass::Socket
            }
        },
        SocketDomain::Vsock => SocketClass::Vsock,
        SocketDomain::Inet | SocketDomain::Inet6 => match socket.socket_type {
            SocketType::Stream => match socket.protocol {
                SocketProtocol::IP | SocketProtocol::TCP => SocketClass::Tcp,
                _ => SocketClass::Socket,
            },
            SocketType::Datagram => match socket.protocol {
                SocketProtocol::IP | SocketProtocol::UDP => SocketClass::Udp,
                _ => SocketClass::Socket,
            },
            SocketType::Raw
            | SocketType::Rdm
            | SocketType::SeqPacket
            | SocketType::Dccp
            | SocketType::Packet => SocketClass::RawIp,
        },
        SocketDomain::Netlink => match NetlinkFamily::from_raw(socket.protocol.as_raw()) {
            NetlinkFamily::Route => SocketClass::NetlinkRoute,
            NetlinkFamily::Firewall => SocketClass::NetlinkFirewall,
            NetlinkFamily::SockDiag => SocketClass::NetlinkTcpDiag,
            NetlinkFamily::Nflog => SocketClass::NetlinkNflog,
            NetlinkFamily::Xfrm => SocketClass::NetlinkXfrm,
            NetlinkFamily::Selinux => SocketClass::NetlinkSelinux,
            NetlinkFamily::Iscsi => SocketClass::NetlinkIscsi,
            NetlinkFamily::Audit => SocketClass::NetlinkAudit,
            NetlinkFamily::FibLookup => SocketClass::NetlinkFibLookup,
            NetlinkFamily::Connector => SocketClass::NetlinkConnector,
            NetlinkFamily::Netfilter => SocketClass::NetlinkNetfilter,
            NetlinkFamily::Ip6Fw => SocketClass::NetlinkIp6Fw,
            NetlinkFamily::Dnrtmsg => SocketClass::NetlinkDnrt,
            NetlinkFamily::KobjectUevent => SocketClass::NetlinkKobjectUevent,
            NetlinkFamily::Generic => SocketClass::NetlinkGeneric,
            NetlinkFamily::Scsitransport => SocketClass::NetlinkScsitransport,
            NetlinkFamily::Rdma => SocketClass::NetlinkRdma,
            NetlinkFamily::Crypto => SocketClass::NetlinkCrypto,
            // No specific netlink security class equivalent.
            NetlinkFamily::Ecryptfs
            | NetlinkFamily::Smc
            | NetlinkFamily::Usersock
            | NetlinkFamily::Unsupported => SocketClass::Netlink,
        },
        SocketDomain::Packet => SocketClass::Packet,
        SocketDomain::Key => SocketClass::Key,
    };
    socket_node.security_state.lock().class = security_class.into();
}

/// Checks that `current_task` has the right permissions to perform a bind operation on
/// `socket_node`.
pub(in crate::security) fn check_socket_bind_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    socket_node: &FsNode,
    _socket_address: &SocketAddress,
) -> Result<(), Errno> {
    let current_sid = current_task.security_state.lock().current_sid;
    let FsNodeClass::Socket(socket_class) = socket_node.security_state.lock().class else {
        panic!("check_socket_bind_access called for non-Socket class")
    };

    // TODO(https://fxbug.dev/364569010): Add checks for `name_bind` between the socket and the SID
    // of the port number, and for `node_bind` between the socket and the SID of the IP address.
    has_socket_permission(
        &security_server.as_permission_check(),
        current_sid,
        socket_node,
        &CommonSocketPermission::Bind.for_class(socket_class),
        current_task.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::super::get_cached_sid;
    use super::*;
    use crate::security::selinux_hooks::testing::spawn_kernel_with_selinux_hooks_test_policy_and_run;
    use crate::vfs::socket::new_socket_file;
    use starnix_uapi::open_flags::OpenFlags;

    #[fuchsia::test]
    async fn socket_post_create() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let task_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_socket_create_t:s0".into())
                    .expect("invalid security context");
                current_task.security_state.lock().current_sid = task_sid;

                let socket_node = new_socket_file(
                    locked,
                    current_task,
                    SocketDomain::Unix,
                    SocketType::Stream,
                    OpenFlags::RDWR,
                    SocketProtocol::IP,
                )
                .expect("failed to create socket");
                assert_eq!(
                    socket_node.node().security_state.lock().class,
                    SocketClass::UnixStream.into()
                );
                assert_eq!(get_cached_sid(socket_node.node()), Some(task_sid));
            },
        )
    }
}
