// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::vfs::socket::{NetlinkFamily, Socket, SocketDomain, SocketProtocol, SocketType};
use crate::vfs::FsNode;
use selinux::SocketClass;

pub fn socket_post_create(socket: &Socket, socket_node: &FsNode) {
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
