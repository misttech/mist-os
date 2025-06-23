// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::audit::Auditable;
use super::fs_node::compute_new_fs_node_sid;
use super::{
    check_permission, fs_node_effective_sid_and_class, task_effective_sid, todo_check_permission,
};
use crate::security::selinux_hooks::{superblock, FsNodeLabel};
use crate::task::{CurrentTask, Kernel};
use crate::vfs::socket::{
    socket_fs, NetlinkFamily, Socket, SocketAddress, SocketDomain, SocketFile, SocketPeer,
    SocketProtocol, SocketShutdownFlags, SocketType,
};
use crate::vfs::{Anon, DowncastedFile, FsNode};
use crate::TODO_DENY;
use selinux::permission_check::PermissionCheck;
use selinux::{
    CommonFsNodePermission, CommonSocketPermission, FsNodeClass, InitialSid, KernelPermission,
    SecurityId, SecurityServer, SocketClass, UnixStreamSocketPermission,
};
use starnix_logging::{track_stub, BugRef};
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked};
use starnix_uapi::errors::Errno;

/// Checks that `current_task` has the specified `permission` for the `socket_node`.
fn has_socket_permission(
    permission_check: &PermissionCheck<'_>,
    kernel: &Kernel,
    subject_sid: SecurityId,
    socket_node: &FsNode,
    permission: KernelPermission,
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
        kernel,
        subject_sid,
        socket_sid,
        permission,
        (&audit_context).into(),
    )
}

/// Checks that `current_task` has the specified `permission` for the `socket_sid`.
fn has_socket_permission_for_sid(
    permission_check: &PermissionCheck<'_>,
    kernel: &Kernel,
    subject_sid: SecurityId,
    socket_sid: SecurityId,
    permission: KernelPermission,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    // If the socket is for kernel-internal use we can return success immediately.
    // TODO: https://fxbug.dev/364569010 - check if there are additional cases when the socket is
    // for kernel-internal use.
    if subject_sid == InitialSid::Kernel.into() || socket_sid == InitialSid::Kernel.into() {
        return Ok(());
    }
    check_permission(permission_check, kernel, subject_sid, socket_sid, permission, audit_context)
}

/// Checks that `current_task` has the specified `permission` for the `socket_node`, with
/// "todo_deny" on denial.
fn todo_has_socket_permission(
    bug: BugRef,
    permission_check: &PermissionCheck<'_>,
    kernel: &Kernel,
    subject_sid: SecurityId,
    socket_node: &FsNode,
    permission: KernelPermission,
    audit_context: Auditable<'_>,
) -> Result<(), Errno> {
    // Permissions are allowed for kernel sockets.
    let socket_sid = fs_node_effective_sid_and_class(socket_node).sid;

    // TODO: https://fxbug.dev/364569010 - check if there are additional cases when the socket is
    // for kernel-internal use.
    if Anon::is_private(socket_node)
        || subject_sid == InitialSid::Kernel.into()
        || socket_sid == InitialSid::Kernel.into()
    {
        return Ok(());
    }

    let audit_context = [audit_context, socket_node.into()];
    todo_check_permission(
        bug,
        kernel,
        permission_check,
        subject_sid,
        socket_sid,
        permission,
        (&audit_context).into(),
    )
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
                _ => SocketClass::RawIp,
            },
            SocketType::Datagram => match protocol {
                SocketProtocol::IP | SocketProtocol::UDP => SocketClass::Udp,
                _ => SocketClass::RawIp,
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
pub(in crate::security) fn check_socket_create_access<L>(
    locked: &mut Locked<L>,
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    domain: SocketDomain,
    socket_type: SocketType,
    protocol: SocketProtocol,
    kernel_private: bool,
) -> Result<(), Errno>
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    // Creating kernel sockets is allowed.
    if kernel_private {
        return Ok(());
    }

    let sockfs = socket_fs(current_task.kernel());
    // Ensure sockfs gets labeled, in case it was mounted after the SELinux policy has been loaded.
    superblock::file_system_resolve_security(locked, security_server, &current_task, &sockfs)
        .expect("resolve fs security");
    let effective_sid = task_effective_sid(current_task);
    let new_socket_class = compute_socket_security_class(domain, socket_type, protocol);
    let new_socket_sid = if let Some(fs_label) = sockfs.security_state.state.label() {
        compute_new_fs_node_sid(
            security_server,
            current_task,
            fs_label,
            None,
            new_socket_class.into(),
            "".into(),
        )?
    } else {
        // TODO: https://fxbug.dev/364569053 - default to socket-related initial SIDs.
        InitialSid::Unlabeled.into()
    };

    has_socket_permission_for_sid(
        &security_server.as_permission_check(),
        current_task.kernel(),
        effective_sid,
        new_socket_sid,
        CommonFsNodePermission::Create.for_class(new_socket_class),
        current_task.into(),
    )
}

/// Sets the peer security context for each socket in the pair.
pub(in crate::security) fn socket_socketpair(
    left: DowncastedFile<'_, SocketFile>,
    right: DowncastedFile<'_, SocketFile>,
) -> Result<(), Errno> {
    let left_label = left.file().node().security_state.lock().label.clone();
    let FsNodeLabel::SecurityId { sid: left_sid } = left_label else {
        panic!("SecurityId not set for socketpair")
    };
    let right_label = right.file().node().security_state.lock().label.clone();
    let FsNodeLabel::SecurityId { sid: right_sid } = right_label else {
        panic!("SecurityId not set for socketpair")
    };
    *left.socket().security.state.peer_sid.lock() = Some(right_sid);
    *right.socket().security.state.peer_sid.lock() = Some(left_sid);
    Ok(())
}

/// Computes and sets the security class for `socket`.
pub(in crate::security) fn socket_post_create(socket: &Socket) {
    let socket_node = socket.fs_node().expect("socket_post_create without FsNode");
    socket_node.security_state.lock().class =
        compute_socket_security_class(socket.domain, socket.socket_type, socket.protocol).into();
}

/// Checks that `current_task` has the right permissions to perform a bind operation on
/// `socket`.
pub(in crate::security) fn check_socket_bind_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    socket: &Socket,
    _socket_address: &SocketAddress,
) -> Result<(), Errno> {
    let Some(socket_node) = socket.fs_node() else {
        track_stub!(TODO("https://fxbug.dev/414583985"), "check_socket_bind_access without FsNode");
        return Ok(());
    };

    let current_sid = task_effective_sid(current_task);
    let FsNodeClass::Socket(socket_class) = socket_node.security_state.lock().class else {
        panic!("check_socket_bind_access called for non-Socket class")
    };

    // TODO: https://fxbug.dev/364569010 - Add checks for `name_bind` between the socket and the SID
    // of the port number, and for `node_bind` between the socket and the SID of the IP address.
    has_socket_permission(
        &security_server.as_permission_check(),
        current_task.kernel(),
        current_sid,
        &socket_node,
        CommonSocketPermission::Bind.for_class(socket_class),
        current_task.into(),
    )
}

/// Checks that `current_task` has the right permissions to initiate a connection with
/// `socket`.
pub(in crate::security) fn check_socket_connect_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    socket: DowncastedFile<'_, SocketFile>,
    _socket_peer: &SocketPeer,
) -> Result<(), Errno> {
    let current_sid = task_effective_sid(current_task);
    let FsNodeClass::Socket(socket_class) = socket.file().node().security_state.lock().class else {
        panic!("check_socket_connect_access called for non-Socket class")
    };

    // TODO: https://fxbug.dev/364568577 - Add checks for `name_connect` between the socket and the
    // SID of the port number for TCP sockets.
    has_socket_permission(
        &security_server.as_permission_check(),
        current_task.kernel(),
        current_sid,
        &socket.file().node(),
        CommonSocketPermission::Connect.for_class(socket_class),
        current_task.into(),
    )
}

/// Checks that `current_task` has permission to listen on `socket`.
pub(in crate::security) fn check_socket_listen_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    socket: &Socket,
    _backlog: i32,
) -> Result<(), Errno> {
    let Some(socket_node) = socket.fs_node() else {
        track_stub!(
            TODO("https://fxbug.dev/414583985"),
            "check_socket_listen_access without FsNode"
        );
        return Ok(());
    };

    let current_sid = task_effective_sid(current_task);
    let FsNodeClass::Socket(socket_class) = socket_node.security_state.lock().class else {
        panic!("check_socket_listen_access called for non-Socket class")
    };

    todo_has_socket_permission(
        TODO_DENY!("https://fxbug.dev/411396154", "Enforce socket_listen checks."),
        &security_server.as_permission_check(),
        current_task.kernel(),
        current_sid,
        &socket_node,
        CommonSocketPermission::Listen.for_class(socket_class),
        current_task.into(),
    )
}

/// Checks that `current_task` has permission to accept a connection on `listening_socket`, and
/// sets the security state for `accepted_socket` to match the context of `listening_socket`.
pub(in crate::security) fn socket_accept(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    listening_socket: DowncastedFile<'_, SocketFile>,
    accepted_socket: DowncastedFile<'_, SocketFile>,
) -> Result<(), Errno> {
    let current_sid = task_effective_sid(current_task);
    let listening_security_state = listening_socket.file().node().security_state.lock().clone();
    let FsNodeClass::Socket(socket_class) = listening_security_state.class else {
        panic!("socket_accept called for non-Socket class")
    };

    todo_has_socket_permission(
        TODO_DENY!("https://fxbug.dev/411396154", "Enforce socket_accept checks."),
        &security_server.as_permission_check(),
        current_task.kernel(),
        current_sid,
        &listening_socket.file().node(),
        CommonSocketPermission::Accept.for_class(socket_class),
        current_task.into(),
    )?;
    *accepted_socket.file().node().security_state.lock() = listening_security_state;
    Ok(())
}

/// Checks that `current_task` has permission to get socket options on `socket`.
pub(in crate::security) fn check_socket_getsockopt_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    socket: &Socket,
    _level: u32,
    _optname: u32,
) -> Result<(), Errno> {
    let Some(socket_node) = socket.fs_node() else {
        track_stub!(
            TODO("https://fxbug.dev/414583985"),
            "check_socket_getsockopt_access without FsNode"
        );
        return Ok(());
    };
    let current_sid = task_effective_sid(current_task);
    let FsNodeClass::Socket(socket_class) = socket_node.security_state.lock().class else {
        panic!("check_socket_getsockopt_access called for non-Socket class")
    };

    todo_has_socket_permission(
        TODO_DENY!("https://fxbug.dev/411396154", "Enforce socket_getsockopt checks."),
        &security_server.as_permission_check(),
        current_task.kernel(),
        current_sid,
        &socket_node,
        CommonSocketPermission::GetOpt.for_class(socket_class),
        current_task.into(),
    )
}

/// Checks that `current_task` has permission to set socket options on `socket`.
pub(in crate::security) fn check_socket_setsockopt_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    socket: &Socket,
    _level: u32,
    _optname: u32,
) -> Result<(), Errno> {
    let Some(socket_node) = socket.fs_node() else {
        track_stub!(
            TODO("https://fxbug.dev/414583985"),
            "check_socket_setsockopt_access without FsNode"
        );
        return Ok(());
    };
    let current_sid = task_effective_sid(current_task);
    let FsNodeClass::Socket(socket_class) = socket_node.security_state.lock().class else {
        panic!("check_socket_setsockopt_access called for non-Socket class")
    };

    todo_has_socket_permission(
        TODO_DENY!("https://fxbug.dev/411396154", "Enforce socket_setsockopt checks."),
        &security_server.as_permission_check(),
        current_task.kernel(),
        current_sid,
        &socket_node,
        CommonSocketPermission::SetOpt.for_class(socket_class),
        current_task.into(),
    )
}

/// Checks that `current_task` has permission to send a message on `socket`.
pub(in crate::security) fn check_socket_sendmsg_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    socket: &Socket,
) -> Result<(), Errno> {
    let Some(socket_node) = socket.fs_node() else {
        track_stub!(
            TODO("https://fxbug.dev/414583985"),
            "check_socket_sendmsg_access without FsNode"
        );
        return Ok(());
    };
    let current_sid = current_task.security_state.lock().current_sid;
    let FsNodeClass::Socket(socket_class) = socket_node.security_state.lock().class else {
        panic!(
            "check_socket_sendmsg_access called for non-Socket class {:?}",
            socket_node.security_state.lock().class
        )
    };

    todo_has_socket_permission(
        TODO_DENY!("https://fxbug.dev/411396154", "Enforce socket_sendmsg checks."),
        &security_server.as_permission_check(),
        current_task.kernel(),
        current_sid,
        &socket_node,
        CommonFsNodePermission::Write.for_class(socket_class),
        current_task.into(),
    )
}

/// Checks that `current_task` has permission to receive a message on `socket`.
pub(in crate::security) fn check_socket_recvmsg_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    socket: &Socket,
) -> Result<(), Errno> {
    let Some(socket_node) = socket.fs_node() else {
        track_stub!(
            TODO("https://fxbug.dev/414583985"),
            "check_socket_recvmsg_access without FsNode"
        );
        return Ok(());
    };
    let current_sid = current_task.security_state.lock().current_sid;
    let FsNodeClass::Socket(socket_class) = socket_node.security_state.lock().class else {
        panic!(
            "check_socket_recvmsg_access called for non-Socket class {:?}",
            socket_node.security_state.lock().class
        )
    };

    todo_has_socket_permission(
        TODO_DENY!("https://fxbug.dev/411396154", "Enforce socket_recvmsg checks."),
        &security_server.as_permission_check(),
        current_task.kernel(),
        current_sid,
        &socket_node,
        CommonFsNodePermission::Read.for_class(socket_class),
        current_task.into(),
    )
}

/// Checks that `current_task` has permission to get the name of `socket`.
pub(in crate::security) fn check_socket_getname_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    socket: &Socket,
) -> Result<(), Errno> {
    let Some(socket_node) = socket.fs_node() else {
        track_stub!(
            TODO("https://fxbug.dev/414583985"),
            "check_socket_getname_access without FsNode"
        );
        return Ok(());
    };
    let current_sid = task_effective_sid(current_task);
    let FsNodeClass::Socket(socket_class) = socket_node.security_state.lock().class else {
        panic!("check_socket_getname_access called for non-Socket class")
    };

    todo_has_socket_permission(
        TODO_DENY!("https://fxbug.dev/411396154", "Enforce socket_getname checks."),
        &security_server.as_permission_check(),
        current_task.kernel(),
        current_sid,
        &socket_node,
        CommonFsNodePermission::GetAttr.for_class(socket_class),
        current_task.into(),
    )
}

/// Checks that `current_task` has permission to shutdown `socket`.
pub(in crate::security) fn check_socket_shutdown_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    socket: &Socket,
    _how: SocketShutdownFlags,
) -> Result<(), Errno> {
    let Some(socket_node) = socket.fs_node() else {
        track_stub!(
            TODO("https://fxbug.dev/414583985"),
            "check_socket_shutdown_access without FsNode"
        );
        return Ok(());
    };

    let current_sid = task_effective_sid(current_task);
    let FsNodeClass::Socket(socket_class) = socket_node.security_state.lock().class else {
        panic!("check_socket_shutdown_access called for non-Socket class")
    };

    todo_has_socket_permission(
        TODO_DENY!("https://fxbug.dev/411396154", "Enforce socket_shutdown checks."),
        &security_server.as_permission_check(),
        current_task.kernel(),
        current_sid,
        &socket_node,
        CommonSocketPermission::Shutdown.for_class(socket_class),
        current_task.into(),
    )
}

/// Returns the Security Context with which the [`crate::vfs::Socket`]'s peer is labeled.
pub(in crate::security) fn socket_getpeersec_stream(
    security_server: &SecurityServer,
    _current_task: &CurrentTask,
    socket: &Socket,
) -> Result<Vec<u8>, Errno> {
    let peer_sid = socket.security.state.peer_sid.lock().unwrap_or(InitialSid::Unlabeled.into());
    Ok(security_server.sid_to_security_context(peer_sid).unwrap())
}

/// Checks if the Unix domain `sending_socket` is allowed to send a message to the
/// `receiving_socket`.
pub(in crate::security) fn unix_may_send(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    sending_socket: &Socket,
    receiving_socket: &Socket,
) -> Result<(), Errno> {
    let (Some(sending_node), Some(receiving_node)) =
        (sending_socket.fs_node(), receiving_socket.fs_node())
    else {
        track_stub!(TODO("https://fxbug.dev/414583985"), "unix_may_send without FsNode");
        return Ok(());
    };

    let sending_sid = fs_node_effective_sid_and_class(&sending_node).sid;
    let receiving_class = fs_node_effective_sid_and_class(&receiving_node).class;
    let FsNodeClass::Socket(receiving_class) = receiving_class else {
        panic!("unix_may_send called for non-Socket class")
    };

    has_socket_permission(
        &security_server.as_permission_check(),
        current_task.kernel(),
        sending_sid,
        &receiving_node,
        CommonSocketPermission::SendTo.for_class(receiving_class),
        current_task.into(),
    )
}

/// Checks if the Unix domain `client_socket` is allowed to connect to `listening_sock`, and
/// initializes security state for the client and server sockets.
pub(in crate::security) fn unix_stream_connect(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    client_socket: &Socket,
    listening_socket: &Socket,
    server_socket: &Socket,
) -> Result<(), Errno> {
    let (Some(client_node), Some(listening_node)) =
        (client_socket.fs_node(), listening_socket.fs_node())
    else {
        track_stub!(TODO("https://fxbug.dev/414583985"), "unix_stream_connect without FsNode");
        return Ok(());
    };

    // Verify whether the `client_socket` has permission to connect to the `listening_socket`.
    let client_sid = fs_node_effective_sid_and_class(&client_node).sid;
    todo_has_socket_permission(
        TODO_DENY!("https://fxbug.dev/364569156", "Enforce unix_stream_connect"),
        &security_server.as_permission_check(),
        current_task.kernel(),
        client_sid,
        &listening_node,
        UnixStreamSocketPermission::ConnectTo.into(),
        current_task.into(),
    )?;

    // Permission is granted, so populate the `peer_sid` of the client & server sockets with one
    // another's SIDs, for e.g. `SO_GETPEERSEC` to return.
    // TODO: https://fxbug.dev/414583985 - the `server_socket` does not yet have an associated
    // `FsNode`, nor security label, so the `listening_socket` label must be used for now.
    let listening_sid = fs_node_effective_sid_and_class(&listening_node).sid;
    *client_socket.security.state.peer_sid.lock() = Some(listening_sid);
    *server_socket.security.state.peer_sid.lock() = Some(client_sid);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::get_cached_sid;
    use super::*;
    use crate::security::selinux_hooks::testing::spawn_kernel_with_selinux_hooks_test_policy_and_run;
    use crate::vfs::socket::SocketFile;
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
                current_task.security_state.lock().effective_sid = task_sid;

                let socket_node = SocketFile::new_socket(
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
                current_task.security_state.lock().effective_sid = task_sid;

                assert_matches!(
                    SocketFile::new_socket(
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
                current_task.security_state.lock().effective_sid = task_sid;

                assert_matches!(SocketFile::new_socket(
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
