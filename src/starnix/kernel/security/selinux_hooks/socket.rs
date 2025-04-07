// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::audit::Auditable;
use super::fs_node::compute_new_fs_node_sid;
use crate::task::CurrentTask;
use crate::vfs::socket::{
    NetlinkFamily, Socket, SocketAddress, SocketDomain, SocketProtocol, SocketType,
};
use crate::vfs::{Anon, FileSystemHandle, FsNode};
use selinux::permission_check::PermissionCheck;
use selinux::{
    CommonFsNodePermission, CommonSocketPermission, FsNodeClass, InitialSid, KernelPermission,
    SecurityId, SecurityServer, SocketClass,
};
use starnix_uapi::errors::Errno;

use super::{check_permission, fs_node_effective_sid_and_class};

/// Checks that `current_task` has the specified `permission` for the `socket_node`.
fn has_socket_permission(
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    socket_node: &FsNode,
    permission: &KernelPermission,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    // Permissions are allowed for kernel sockets.
    if Anon::is_private(socket_node) {
        return Ok(());
    }

    let socket_sid = fs_node_effective_sid_and_class(socket_node).sid;
    let audit_context = [audit_context, socket_node.into()];
    has_socket_permission_for_sid(
        permission_check,
        subject_sid,
        socket_sid,
        permission,
        (&audit_context).into(),
    )
}

/// Checks that `current_task` has the specified `permission` for the `socket_sid`.
fn has_socket_permission_for_sid(
    permission_check: &PermissionCheck<'_>,
    subject_sid: SecurityId,
    socket_sid: SecurityId,
    permission: &KernelPermission,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    // If the socket is for kernel-internal use we can return success immediately.
    // TODO: https://fxbug.dev/364569010 - check if there are additional cases when the socket is
    // for kernel-internal use.
    if subject_sid == SecurityId::initial(InitialSid::Kernel)
        || socket_sid == SecurityId::initial(InitialSid::Kernel)
    {
        return Ok(());
    }
    check_permission(permission_check, subject_sid, socket_sid, permission.clone(), audit_context)
}

/// Computes the socket security class for `domain`, `socket_type` and `protocol`.
fn compute_socket_security_class(
    domain: SocketDomain,
    socket_type: SocketType,
    protocol: SocketProtocol,
) -> SocketClass {
    match domain {
        SocketDomain::Unix => match socket_type {
            SocketType::Stream | SocketType::SeqPacket => SocketClass::UnixStream,
            SocketType::Datagram => SocketClass::UnixDgram,
            SocketType::Raw | SocketType::Rdm | SocketType::Dccp | SocketType::Packet => {
                SocketClass::Socket
            }
        },
        SocketDomain::Vsock => SocketClass::Vsock,
        SocketDomain::Inet | SocketDomain::Inet6 => match socket_type {
            SocketType::Stream => match protocol {
                SocketProtocol::IP | SocketProtocol::TCP => SocketClass::Tcp,
                _ => SocketClass::Socket,
            },
            SocketType::Datagram => match protocol {
                SocketProtocol::IP | SocketProtocol::UDP => SocketClass::Udp,
                _ => SocketClass::Socket,
            },
            SocketType::Raw
            | SocketType::Rdm
            | SocketType::SeqPacket
            | SocketType::Dccp
            | SocketType::Packet => SocketClass::RawIp,
        },
        SocketDomain::Netlink => match NetlinkFamily::from_raw(protocol.as_raw()) {
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
    }
}

/// Checks that `current_task` has permission to create a socket with `domain`, `socket_type` and
/// `protocol`.
pub(in crate::security) fn check_socket_create_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    domain: SocketDomain,
    socket_type: SocketType,
    protocol: SocketProtocol,
    sockfs: &FileSystemHandle,
    kernel_private: bool,
) -> Result<(), Errno> {
    // Creating kernel sockets is allowed.
    if kernel_private {
        return Ok(());
    }

    let current_sid = current_task.security_state.lock().current_sid;
    let new_socket_class = compute_socket_security_class(domain, socket_type, protocol);
    let new_socket_sid = compute_new_fs_node_sid(
        security_server,
        current_task,
        &sockfs,
        None,
        new_socket_class.into(),
        "".into(),
    )?
    .map(|(sid, _)| sid)
    // TODO: https://fxbug.dev/364569053 - default to socket-related initial SIDs.
    .unwrap_or_else(|| SecurityId::initial(InitialSid::Unlabeled));

    has_socket_permission_for_sid(
        &security_server.as_permission_check(),
        current_sid,
        new_socket_sid,
        &CommonFsNodePermission::Create.for_class(new_socket_class),
        current_task.into(),
    )
}

/// Computes and sets the security class for `socket_node`.
pub(in crate::security) fn socket_post_create(socket: &Socket, socket_node: &FsNode) {
    socket_node.security_state.lock().class =
        compute_socket_security_class(socket.domain, socket.socket_type, socket.protocol).into();
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

    // TODO: https://fxbug.dev/364569010 - Add checks for `name_bind` between the socket and the SID
    // of the port number, and for `node_bind` between the socket and the SID of the IP address.
    has_socket_permission(
        &security_server.as_permission_check(),
        current_sid,
        socket_node,
        &CommonSocketPermission::Bind.for_class(socket_class),
        current_task.into(),
    )
}

/// Checks that `current_task` has the right permissions to initiate a connection with
/// `socket_node`.
pub(in crate::security) fn check_socket_connect_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    socket_node: &FsNode,
    _socket_address: &SocketAddress,
) -> Result<(), Errno> {
    let current_sid = current_task.security_state.lock().current_sid;
    let FsNodeClass::Socket(socket_class) = socket_node.security_state.lock().class else {
        panic!("check_socket_connect_access called for non-Socket class")
    };

    // TODO: https://fxbug.dev/364568577 - Add checks for `name_connect` between the socket and the
    // SID of the port number for TCP sockets.
    has_socket_permission(
        &security_server.as_permission_check(),
        current_sid,
        socket_node,
        &CommonSocketPermission::Connect.for_class(socket_class),
        current_task.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::super::get_cached_sid;
    use super::*;
    use crate::security::selinux_hooks::testing::spawn_kernel_with_selinux_hooks_test_policy_and_run;
    use crate::vfs::socket::new_socket_file;
    use assert_matches::assert_matches;
    use starnix_uapi::errors::EACCES;
    use starnix_uapi::open_flags::OpenFlags;

    #[fuchsia::test]
    async fn socket_post_create() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let task_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_socket_create_yes_t:s0".into())
                    .expect("invalid security context");
                current_task.security_state.lock().current_sid = task_sid;

                let socket_node = new_socket_file(
                    locked,
                    current_task,
                    SocketDomain::Unix,
                    SocketType::Stream,
                    OpenFlags::RDWR,
                    SocketProtocol::IP,
                    /* kernel_private=*/ false,
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

    #[fuchsia::test]
    async fn socket_create_is_allowed() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let task_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_socket_create_yes_t:s0".into())
                    .expect("invalid security context");
                current_task.security_state.lock().current_sid = task_sid;

                assert_matches!(
                    new_socket_file(
                        locked,
                        current_task,
                        SocketDomain::Unix,
                        SocketType::Stream,
                        OpenFlags::RDWR,
                        SocketProtocol::IP,
                        /* kernel_private= */ false,
                    ),
                    Ok(_)
                );
            },
        )
    }

    #[fuchsia::test]
    async fn socket_create_is_denied() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let task_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_socket_create_no_t:s0".into())
                    .expect("invalid security context");
                current_task.security_state.lock().current_sid = task_sid;

                assert_matches!(new_socket_file(
                    locked,
                    current_task,
                    SocketDomain::Unix,
                    SocketType::Stream,
                    OpenFlags::RDWR,
                    SocketProtocol::IP,
                    /* kernel_private= */ false,
                ), Err(errno) if errno == EACCES);
            },
        )
    }
}
