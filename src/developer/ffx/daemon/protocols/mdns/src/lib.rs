// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use ffx_config::get;
use ffx_stream_util::TryStreamUtilExt;
use fidl::endpoints::ProtocolMarker;
use fidl_fuchsia_developer_ffx as ffx;
use fuchsia_async::Task;
use futures::TryStreamExt;
use mdns_discovery::{
    discovery_loop, DiscoveryConfig, MdnsEnabledChecker, MdnsProtocol, MDNS_BROADCAST_INTERVAL,
    MDNS_INTERFACE_DISCOVERY_INTERVAL, MDNS_TTL,
};
use protocols::prelude::*;
use std::rc::Rc;

// Default port to listen on for MDNS queries
const MDNS_PORT: u16 = 5353;

#[ffx_protocol]
#[derive(Default)]
pub struct Mdns {
    mdns_task: Option<Task<()>>,
    inner: Option<Rc<MdnsProtocol>>,
    receiver: Option<async_channel::Receiver<ffx::MdnsEventType>>,
    // If None defaults to MDNS_PORT
    mdns_port: Option<u16>,
}

struct ConfigLoader;

#[async_trait(?Send)]
impl MdnsEnabledChecker for ConfigLoader {
    async fn enabled(&self) -> bool {
        get("discovery.mdns.enabled").unwrap_or(true)
    }
}

#[async_trait(?Send)]
impl FidlProtocol for Mdns {
    type Protocol = ffx::MdnsMarker;
    type StreamHandler = FidlStreamHandler<Self>;

    async fn handle(&self, _cx: &Context, req: ffx::MdnsRequest) -> Result<()> {
        match req {
            ffx::MdnsRequest::GetTargets { responder } => responder
                .send(
                    &self.inner.as_ref().expect("inner state should be initalized").target_cache(),
                )
                .map_err(Into::into),
            ffx::MdnsRequest::GetNextEvent { responder } => responder
                .send(
                    self.receiver
                        .as_ref()
                        .expect("receiver should be initialized")
                        .recv()
                        .await
                        .ok()
                        .as_ref(),
                )
                .map_err(Into::into),
        }
    }

    async fn start(&mut self, _cx: &Context) -> Result<()> {
        let (sender, receiver) = async_channel::bounded::<ffx::MdnsEventType>(1);
        let inner = Rc::new(MdnsProtocol { events_out: sender, target_cache: Default::default() });
        self.inner.replace(inner.clone());
        self.receiver.replace(receiver);
        let inner = Rc::downgrade(&inner);
        let mdns_port = self.mdns_port.unwrap_or(MDNS_PORT);
        self.mdns_task.replace(Task::local(discovery_loop(
            DiscoveryConfig {
                socket_tasks: Default::default(),
                mdns_protocol: inner,
                discovery_interval: MDNS_INTERFACE_DISCOVERY_INTERVAL,
                query_interval: MDNS_BROADCAST_INTERVAL,
                ttl: MDNS_TTL,
                mdns_port,
            },
            ConfigLoader {},
        )));
        Ok(())
    }

    async fn stop(&mut self, _cx: &Context) -> Result<()> {
        self.mdns_task.take().ok_or_else(|| anyhow!("mdns_task never started"))?;
        Ok(())
    }

    async fn serve<'a>(
        &'a self,
        cx: &'a Context,
        stream: <Self::Protocol as ProtocolMarker>::RequestStream,
    ) -> Result<()> {
        // This is necessary as we'll be hanging forever waiting on incoming
        // traffic. This will exit early if the stream is closed at any point.
        stream
            .map_err(|err| anyhow!("{}", err))
            .try_for_each_concurrent_while_connected(None, |req| self.handle(cx, req))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lazy_static::lazy_static;
    use mdns::protocol::{
        Class, DomainBuilder, EmbeddedPacketBuilder, MessageBuilder, RecordBuilder, Type,
    };
    use packet::serialize::{InnerPacketBuilder, Serializer};
    use protocols::testing::FakeDaemonBuilder;
    use std::cell::RefCell;
    use std::net::{IpAddr, SocketAddr};

    lazy_static! {
        // This is copied from the //fuchsia/lib/src/mdns/rust/src/protocol.rs
        // tests library.
        static ref MDNS_PACKET: Vec<u8> = vec![
            0x00, 0x00, 0x84, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x04, 0x08, 0x5f,
            0x66, 0x75, 0x63, 0x68, 0x73, 0x69, 0x61, 0x04, 0x5f, 0x75, 0x64, 0x70, 0x05, 0x6c,
            0x6f, 0x63, 0x61, 0x6c, 0x00, 0x00, 0x0c, 0x00, 0x01, 0x00, 0x00, 0x11, 0x94, 0x00,
            0x18, 0x15, 0x74, 0x68, 0x75, 0x6d, 0x62, 0x2d, 0x73, 0x65, 0x74, 0x2d, 0x68, 0x75,
            0x6d, 0x61, 0x6e, 0x2d, 0x73, 0x68, 0x72, 0x65, 0x64, 0xc0, 0x0c, 0xc0, 0x2b, 0x00,
            0x21, 0x80, 0x01, 0x00, 0x00, 0x00, 0x78, 0x00, 0x1e, 0x00, 0x00, 0x00, 0x00, 0x14,
            0xe9, 0x15, 0x74, 0x68, 0x75, 0x6d, 0x62, 0x2d, 0x73, 0x65, 0x74, 0x2d, 0x68, 0x75,
            0x6d, 0x61, 0x6e, 0x2d, 0x73, 0x68, 0x72, 0x65, 0x64, 0xc0, 0x1a, 0xc0, 0x2b, 0x00,
            0x10, 0x80, 0x01, 0x00, 0x00, 0x11, 0x94, 0x00, 0x01, 0x00, 0xc0, 0x55, 0x00, 0x01,
            0x80, 0x01, 0x00, 0x00, 0x00, 0x78, 0x00, 0x04, 0xac, 0x10, 0xf3, 0x26, 0xc0, 0x55,
            0x00, 0x1c, 0x80, 0x01, 0x00, 0x00, 0x00, 0x78, 0x00, 0x10, 0xfe, 0x80, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x8e, 0xae, 0x4c, 0xff, 0xfe, 0xe9, 0xc9, 0xd3,
        ];
    }

    async fn wait_for_port_binds(proxy: &ffx::MdnsProxy) -> u16 {
        if let Some(e) = proxy.get_next_event().await.unwrap() {
            match *e {
                ffx::MdnsEventType::SocketBound(ffx::MdnsBindEvent { port, .. }) => {
                    let p = port.unwrap();
                    assert_ne!(p, 0);
                    return p;
                }
                e => panic!(
                    "events should start with two port binds. encountered unrecognized event: {:?}",
                    e
                ),
            }
        }
        panic!("no port bound");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_mdns_get_targets_empty() {
        let mdns = Rc::new(RefCell::new(Mdns {
            inner: None,
            receiver: None,
            mdns_task: None,
            mdns_port: Some(0),
        }));
        let daemon = FakeDaemonBuilder::new().inject_fidl_protocol::<Mdns>(mdns).build();
        let proxy = daemon.open_proxy::<ffx::MdnsMarker>().await;
        let targets = proxy.get_targets().await.unwrap();
        assert_eq!(targets.len(), 0);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_mdns_stop() {
        let daemon = FakeDaemonBuilder::new().build();
        let protocol = Rc::new(RefCell::new(Mdns {
            mdns_task: None,
            inner: None,
            receiver: None,
            mdns_port: Some(0),
        }));
        let (proxy, task) = protocols::testing::create_proxy(protocol.clone(), &daemon).await;
        drop(proxy);
        task.await;
        assert!(protocol.borrow().mdns_task.is_none());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_mdns_bind_event_on_first_listen() {
        let mdns = Rc::new(RefCell::new(Mdns {
            inner: None,
            receiver: None,
            mdns_task: None,
            mdns_port: Some(0),
        }));
        let daemon = FakeDaemonBuilder::new().inject_fidl_protocol::<Mdns>(mdns).build();
        let proxy = daemon.open_proxy::<ffx::MdnsMarker>().await;
        while let Some(e) = proxy.get_next_event().await.unwrap() {
            if matches!(*e, ffx::MdnsEventType::SocketBound(_)) {
                break;
            }
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    #[ignore] // TODO(b/297919461) -- re-enable or delete
    async fn test_mdns_network_traffic_valid() {
        let mdns = Rc::new(RefCell::new(Mdns {
            inner: None,
            receiver: None,
            mdns_task: None,
            mdns_port: Some(0),
        }));
        let daemon = FakeDaemonBuilder::new().inject_fidl_protocol::<Mdns>(mdns).build();
        let proxy = daemon.open_proxy::<ffx::MdnsMarker>().await;
        let bound_port = wait_for_port_binds(&proxy).await;

        // Note: this and other tests are only using IPv4 due to some issues
        // on Mac, wherein sending on the unspecified address leads to a "no
        // route to host" error. For some reason using IPv6 with either the
        // unspecified address or the localhost address leads in errors or
        // hangs on Mac tests.
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )
        .unwrap();
        socket.set_ttl(1).unwrap();
        socket.set_multicast_ttl_v4(1).unwrap();
        let addr: SocketAddr = (std::net::Ipv4Addr::LOCALHOST, bound_port).into();

        let my_ip = match addr.ip() {
            IpAddr::V4(addr) => addr.octets(),
            _ => panic!("expected ipv4 addr"),
        };

        let mut message = MessageBuilder::new(0, true);
        let domain =
            DomainBuilder::from_str("thumb-set-human-shred._fuchsia._udp.local").unwrap().bytes();

        let ptr = RecordBuilder::new(
            DomainBuilder::from_str("_fuchsia._udp.local").unwrap(),
            Type::Ptr,
            Class::In,
            true,
            1,
            &domain,
        );
        message.add_answer(ptr);

        let nodename = DomainBuilder::from_str("thumb-set-human-shred.local").unwrap();
        let rec = RecordBuilder::new(nodename, Type::A, Class::In, true, 4500, &my_ip);
        message.add_additional(rec);

        let msg_bytes = message
            .into_serializer()
            .serialize_vec_outer()
            .unwrap_or_else(|_| panic!("failed to serialize"))
            .unwrap_b();
        socket.send_to(msg_bytes.as_ref(), &addr.into()).unwrap();

        while let Some(e) = proxy.get_next_event().await.unwrap() {
            if matches!(*e, ffx::MdnsEventType::TargetFound(_),) {
                break;
            }
        }
        assert_eq!(
            proxy.get_targets().await.unwrap().into_iter().next().unwrap().nodename.unwrap(),
            "thumb-set-human-shred",
        );
        socket.send_to(msg_bytes.as_ref(), &addr.into()).unwrap();
        while let Some(e) = proxy.get_next_event().await.unwrap() {
            if matches!(*e, ffx::MdnsEventType::TargetRediscovered(_)) {
                break;
            }
        }
        assert_eq!(
            proxy.get_targets().await.unwrap().into_iter().next().unwrap().nodename.unwrap(),
            "thumb-set-human-shred",
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_mdns_network_traffic_invalid() {
        let mdns = Rc::new(RefCell::new(Mdns {
            inner: None,
            receiver: None,
            mdns_task: None,
            mdns_port: Some(0),
        }));
        let daemon = FakeDaemonBuilder::new().inject_fidl_protocol::<Mdns>(mdns).build();
        let proxy = daemon.open_proxy::<ffx::MdnsMarker>().await;
        let bound_port = wait_for_port_binds(&proxy).await;

        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )
        .unwrap();
        socket.set_ttl(1).unwrap();
        socket.set_multicast_ttl_v4(1).unwrap();
        let addr: SocketAddr = (std::net::Ipv4Addr::UNSPECIFIED, bound_port).into();
        // This is just a copy of the valid mdns packet but with a few bytes altered.
        let packet: Vec<u8> = vec![
            0x00, 0x00, 0x84, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x04, 0x08, 0x5f,
            0x66, 0x75, 0x63, 0x68, 0x73, 0x69, 0x61, 0x04, 0x5f, 0x75, 0x64, 0x70, 0x05, 0x6c,
            0x6f, 0x63, 0x61, 0x6c, 0x00, 0x00, 0x0c, 0x00, 0x01, 0x00, 0x00, 0x11, 0x94, 0x00,
            0x18, 0x95, 0x74, 0x68, 0x75, 0x6d, 0x62, 0x2d, 0x73, 0x65, 0x74, 0x2d, 0x68, 0x75,
            0x6d, 0x61, 0x6e, 0x2d, 0x73, 0x68, 0x72, 0x65, 0x64, 0xc0, 0x0c, 0xc0, 0x2b, 0x00,
            0x21, 0x80, 0x01, 0x00, 0x10, 0x02, 0x78, 0x00, 0x1e, 0x00, 0x00, 0x00, 0x00, 0x14,
            0xe9, 0x15, 0x74, 0x68, 0x75, 0x6d, 0x62, 0x2d, 0x73, 0x65, 0x74, 0x2d, 0x68, 0x75,
            0x6d, 0x61, 0x6e, 0x2d, 0x73, 0x68, 0x72, 0x65, 0x64, 0xc0, 0x1a, 0xc0, 0x2b, 0x00,
            0x10, 0x80, 0x01, 0x00, 0x00, 0x11, 0x94, 0x00, 0x01, 0x00, 0xc0, 0x55, 0x00, 0x01,
            0x80, 0x01, 0x00, 0x00, 0x00, 0x78, 0x00, 0x04, 0xac, 0x10, 0xf3, 0x26, 0xc0, 0x55,
            0x00, 0x1c, 0x80, 0x01, 0x00, 0x00, 0x00, 0x78, 0x00, 0x10, 0xfe, 0x80, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x8e, 0xae, 0x4c, 0xff, 0xfe, 0xe9, 0xc9, 0xd3,
        ];
        socket.send_to(&packet, &addr.into()).unwrap();
        // This is here to un-stick the executor a bit, as otherwise the socket
        // will not get read, and the code being tested will not get exercised.
        //
        // If this ever flakes it will be because somehow valid traffic made it
        // onto the network.
        fuchsia_async::Timer::new(std::time::Duration::from_millis(200)).await;
        assert_eq!(proxy.get_targets().await.unwrap().len(), 0);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_mdns_network_traffic_wrong_protocol() {
        let mdns = Rc::new(RefCell::new(Mdns {
            inner: None,
            receiver: None,
            mdns_task: None,
            mdns_port: Some(0),
        }));
        let daemon = FakeDaemonBuilder::new().inject_fidl_protocol::<Mdns>(mdns).build();
        let proxy = daemon.open_proxy::<ffx::MdnsMarker>().await;
        let bound_port = wait_for_port_binds(&proxy).await;
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )
        .unwrap();
        socket.set_ttl(1).unwrap();
        socket.set_multicast_ttl_v4(1).unwrap();
        let addr: SocketAddr = (std::net::Ipv4Addr::UNSPECIFIED, bound_port).into();

        let domain = DomainBuilder::from_str("_nonsense._udp.local").unwrap();
        let record = RecordBuilder::new(
            domain,
            Type::Ptr,
            Class::In,
            true,
            4500,
            &[0x03, 'f' as u8, 'o' as u8, 'o' as u8, 0],
        );
        let nodename = DomainBuilder::from_str("fuchsia_thing._fuchsia._udp.local").unwrap();
        let other_record =
            RecordBuilder::new(nodename, Type::A, Class::In, true, 4500, &[8, 8, 8, 8]);
        let mut message = MessageBuilder::new(0, true);
        message.add_additional(record);
        message.add_additional(other_record);
        let msg_bytes = message
            .into_serializer()
            .serialize_vec_outer()
            .unwrap_or_else(|_| panic!("failed to serialize"))
            .unwrap_b();
        socket.send_to(msg_bytes.as_ref(), &addr.into()).unwrap();
        // This is here to un-stick the executor a bit, as otherwise the socket
        // will not get read, and the code being tested will not get exercised.
        //
        // If this ever flakes it will be because somehow valid traffic made it
        // onto the network.
        fuchsia_async::Timer::new(std::time::Duration::from_millis(200)).await;
        assert_eq!(proxy.get_targets().await.unwrap().len(), 0);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    #[ignore] // TODO(b/297919461) -- re-enable or delete
    async fn test_new_and_rediscovered_target() {
        let daemon = FakeDaemonBuilder::new().build();
        let protocol = Rc::new(RefCell::new(Mdns {
            inner: None,
            receiver: None,
            mdns_task: None,
            mdns_port: Some(0),
        }));
        let (proxy, _task) = protocols::testing::create_proxy(protocol.clone(), &daemon).await;
        let svc_inner = protocol.borrow().inner.as_ref().unwrap().clone();
        let nodename = "plop".to_owned();
        // Skip port binding.
        let _ = wait_for_port_binds(&proxy).await;
        svc_inner
            .handle_target(
                ffx::TargetInfo { nodename: Some(nodename.clone()), ..Default::default() },
                5000,
            )
            .await;
        if let Some(e) = proxy.get_next_event().await.unwrap() {
            assert!(matches!(*e, ffx::MdnsEventType::TargetFound(_),));
        }
        assert_eq!(
            proxy.get_targets().await.unwrap().into_iter().next().unwrap().nodename.unwrap(),
            nodename
        );
        svc_inner
            .handle_target(
                ffx::TargetInfo { nodename: Some(nodename.clone()), ..Default::default() },
                5000,
            )
            .await;
        if let Some(e) = proxy.get_next_event().await.unwrap() {
            assert!(matches!(*e, ffx::MdnsEventType::TargetRediscovered(_),));
        }
        assert_eq!(
            proxy.get_targets().await.unwrap().into_iter().next().unwrap().nodename.unwrap(),
            nodename
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_eviction() {
        let daemon = FakeDaemonBuilder::new().build();
        let protocol = Rc::new(RefCell::new(Mdns {
            inner: None,
            receiver: None,
            mdns_task: None,
            mdns_port: Some(0),
        }));
        let (proxy, _task) = protocols::testing::create_proxy(protocol.clone(), &daemon).await;
        let svc_inner = protocol.borrow().inner.as_ref().unwrap().clone();
        let nodename = "plop".to_owned();
        // Skip port binding.
        let _ = wait_for_port_binds(&proxy).await;
        svc_inner
            .handle_target(
                ffx::TargetInfo { nodename: Some(nodename.clone()), ..Default::default() },
                1,
            )
            .await;
        // Hangs until the target expires.
        let mut new_target_found = false;
        while let Some(e) = proxy.get_next_event().await.unwrap() {
            match *e {
                ffx::MdnsEventType::TargetExpired(_) => {
                    break;
                }
                ffx::MdnsEventType::TargetFound(t) => {
                    assert_eq!(t.nodename.unwrap(), nodename);
                    new_target_found = true;
                }
                _ => {}
            }
        }
        assert_eq!(proxy.get_targets().await.unwrap().len(), 0);
        assert!(new_target_found);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_eviction_timer_override() {
        let daemon = FakeDaemonBuilder::new().build();
        let protocol = Rc::new(RefCell::new(Mdns {
            inner: None,
            receiver: None,
            mdns_task: None,
            mdns_port: Some(0),
        }));
        let (proxy, _task) = protocols::testing::create_proxy(protocol.clone(), &daemon).await;
        let svc_inner = protocol.borrow().inner.as_ref().unwrap().clone();
        let nodename = "plop".to_owned();
        // Skip port binding.
        let _ = wait_for_port_binds(&proxy).await;
        svc_inner
            .handle_target(
                ffx::TargetInfo { nodename: Some(nodename.clone()), ..Default::default() },
                50000,
            )
            .await;
        while let Some(e) = proxy.get_next_event().await.unwrap() {
            match *e {
                ffx::MdnsEventType::TargetFound(t) => {
                    assert_eq!(t.nodename.unwrap(), nodename);
                    break;
                }
                _ => {}
            }
        }
        svc_inner
            .handle_target(
                ffx::TargetInfo { nodename: Some(nodename.clone()), ..Default::default() },
                1,
            )
            .await;
        while let Some(e) = proxy.get_next_event().await.unwrap() {
            if matches!(*e, ffx::MdnsEventType::TargetExpired(_)) {
                break;
            }
        }
        assert_eq!(proxy.get_targets().await.unwrap().len(), 0);
    }
}
