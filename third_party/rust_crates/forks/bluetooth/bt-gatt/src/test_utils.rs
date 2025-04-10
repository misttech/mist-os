// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::future::{ready, Ready};
use futures::Stream;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use bt_common::{PeerId, Uuid};

use crate::central::ScanResult;
use crate::client::CharacteristicNotification;
use crate::server::{self, LocalService, ReadResponder, ServiceDefinition, WriteResponder};
use crate::{types::*, GattTypes, ServerTypes};

#[derive(Default)]
struct FakePeerServiceInner {
    // Notifier that's used to send out notification.
    notifiers: HashMap<Handle, UnboundedSender<Result<CharacteristicNotification>>>,

    // Characteristics to return when `read_characteristic` and `discover_characteristics` are
    // called.
    characteristics: HashMap<Handle, (Characteristic, Vec<u8>)>,
}

#[derive(Clone)]
pub struct FakePeerService {
    inner: Arc<Mutex<FakePeerServiceInner>>,
}

impl FakePeerService {
    pub fn new() -> Self {
        Self { inner: Arc::new(Mutex::new(Default::default())) }
    }

    // Adds a characteristic so that it can be returned when discover/read method is
    // called.
    // Also triggers sending a characteristic value change notification to be sent.
    pub fn add_characteristic(&mut self, char: Characteristic, value: Vec<u8>) {
        let mut lock = self.inner.lock();
        let handle = char.handle;
        lock.characteristics.insert(handle, (char, value.clone()));
        if let Some(notifier) = lock.notifiers.get_mut(&handle) {
            notifier
                .unbounded_send(Ok(CharacteristicNotification {
                    handle,
                    value,
                    maybe_truncated: false,
                }))
                .expect("should succeed");
        }
    }

    // Sets expected characteristic value so that it can be used for validation when
    // write method is called.
    pub fn expect_characteristic_value(&mut self, handle: &Handle, value: Vec<u8>) {
        let mut lock = self.inner.lock();
        let Some(char) = lock.characteristics.get_mut(handle) else {
            panic!("Can't find characteristic {handle:?} to set expected value");
        };
        char.1 = value;
    }

    /// Sends a notification on the characteristic with the provided `handle`.
    pub fn notify(&self, handle: &Handle, notification: Result<CharacteristicNotification>) {
        let mut lock = self.inner.lock();
        if let Some(notifier) = lock.notifiers.get_mut(handle) {
            notifier.unbounded_send(notification).expect("can send notification");
        }
    }

    /// Removes the notification subscription for the characteristic with the
    /// provided `handle`.
    pub fn clear_notifier(&self, handle: &Handle) {
        let mut lock = self.inner.lock();
        let _ = lock.notifiers.remove(handle);
    }
}

impl crate::client::PeerService<FakeTypes> for FakePeerService {
    fn discover_characteristics(
        &self,
        uuid: Option<Uuid>,
    ) -> <FakeTypes as GattTypes>::CharacteristicDiscoveryFut {
        let lock = self.inner.lock();
        let mut result = Vec::new();
        for (_handle, (char, _value)) in &lock.characteristics {
            match uuid {
                Some(uuid) if uuid == char.uuid => result.push(char.clone()),
                None => result.push(char.clone()),
                _ => {}
            }
        }
        ready(Ok(result))
    }

    fn read_characteristic<'a>(
        &self,
        handle: &Handle,
        _offset: u16,
        buf: &'a mut [u8],
    ) -> <FakeTypes as GattTypes>::ReadFut<'a> {
        let read_characteristics = &(*self.inner.lock()).characteristics;
        let Some((_, value)) = read_characteristics.get(handle) else {
            return ready(Err(Error::Gatt(GattError::InvalidHandle)));
        };
        buf[..value.len()].copy_from_slice(value.as_slice());
        ready(Ok((value.len(), false)))
    }

    // For testing, should call `expect_characteristic_value` with the expected
    // value.
    fn write_characteristic<'a>(
        &self,
        handle: &Handle,
        _mode: WriteMode,
        _offset: u16,
        buf: &'a [u8],
    ) -> <FakeTypes as GattTypes>::WriteFut<'a> {
        let expected_characteristics = &(*self.inner.lock()).characteristics;
        // The write operation was not expected.
        let Some((_, expected)) = expected_characteristics.get(handle) else {
            panic!("Write operation to characteristic {handle:?} was not expected");
        };
        // Value written was not expected.
        if buf.len() != expected.len() || &buf[..expected.len()] != expected.as_slice() {
            panic!("Value written to characteristic {handle:?} was not expected: {buf:?}");
        }
        ready(Ok(()))
    }

    fn read_descriptor<'a>(
        &self,
        _handle: &Handle,
        _offset: u16,
        _buf: &'a mut [u8],
    ) -> <FakeTypes as GattTypes>::ReadFut<'a> {
        todo!()
    }

    fn write_descriptor<'a>(
        &self,
        _handle: &Handle,
        _offset: u16,
        _buf: &'a [u8],
    ) -> <FakeTypes as GattTypes>::WriteFut<'a> {
        todo!()
    }

    fn subscribe(&self, handle: &Handle) -> <FakeTypes as GattTypes>::NotificationStream {
        let (sender, receiver) = unbounded();
        (*self.inner.lock()).notifiers.insert(*handle, sender);
        receiver
    }
}

#[derive(Clone)]
pub struct FakeServiceHandle {
    pub uuid: Uuid,
    pub is_primary: bool,
    pub fake_service: FakePeerService,
}

impl crate::client::PeerServiceHandle<FakeTypes> for FakeServiceHandle {
    fn uuid(&self) -> Uuid {
        self.uuid
    }

    fn is_primary(&self) -> bool {
        self.is_primary
    }

    fn connect(&self) -> <FakeTypes as GattTypes>::ServiceConnectFut {
        futures::future::ready(Ok(self.fake_service.clone()))
    }
}

#[derive(Default)]
struct FakeClientInner {
    fake_services: Vec<FakeServiceHandle>,
}

#[derive(Clone)]
pub struct FakeClient {
    inner: Arc<Mutex<FakeClientInner>>,
}

impl FakeClient {
    pub fn new() -> Self {
        FakeClient { inner: Arc::new(Mutex::new(FakeClientInner::default())) }
    }

    /// Add a fake peer service to this client.
    pub fn add_service(&mut self, uuid: Uuid, is_primary: bool, fake_service: FakePeerService) {
        self.inner.lock().fake_services.push(FakeServiceHandle { uuid, is_primary, fake_service });
    }
}

impl crate::Client<FakeTypes> for FakeClient {
    fn peer_id(&self) -> PeerId {
        todo!()
    }

    fn find_service(&self, uuid: Uuid) -> <FakeTypes as GattTypes>::FindServicesFut {
        let fake_services = &self.inner.lock().fake_services;
        let mut filtered_services = Vec::new();
        for handle in fake_services {
            if handle.uuid == uuid {
                filtered_services.push(handle.clone());
            }
        }

        futures::future::ready(Ok(filtered_services))
    }
}

#[derive(Clone)]
pub struct ScannedResultStream {
    inner: Arc<Mutex<ScannedResultStreamInner>>,
}

#[derive(Default)]
pub struct ScannedResultStreamInner {
    result: Option<Result<ScanResult>>,
}

impl ScannedResultStream {
    pub fn new() -> Self {
        Self { inner: Arc::new(Mutex::new(ScannedResultStreamInner::default())) }
    }

    /// Set scanned result item to output from the stream.
    pub fn set_scanned_result(&mut self, item: Result<ScanResult>) {
        self.inner.lock().result = Some(item);
    }
}

impl Stream for ScannedResultStream {
    type Item = Result<ScanResult>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let mut lock = self.inner.lock();
        if lock.result.is_some() {
            Poll::Ready(lock.result.take())
        } else {
            // Never wake up, as if we never find another result
            Poll::Pending
        }
    }
}

pub struct FakeTypes {}

impl GattTypes for FakeTypes {
    type Central = FakeCentral;
    type ScanResultStream = ScannedResultStream;
    type Client = FakeClient;
    type ConnectFuture = Ready<Result<FakeClient>>;
    type PeerServiceHandle = FakeServiceHandle;
    type FindServicesFut = Ready<Result<Vec<FakeServiceHandle>>>;
    type PeerService = FakePeerService;
    type ServiceConnectFut = Ready<Result<FakePeerService>>;
    type CharacteristicDiscoveryFut = Ready<Result<Vec<Characteristic>>>;
    type NotificationStream = UnboundedReceiver<Result<CharacteristicNotification>>;
    type ReadFut<'a> = Ready<Result<(usize, bool)>>;
    type WriteFut<'a> = Ready<Result<()>>;
}

impl ServerTypes for FakeTypes {
    type Server = FakeServer;
    type LocalService = FakeLocalService;
    type LocalServiceFut = Ready<Result<FakeLocalService>>;
    type ServiceEventStream = UnboundedReceiver<Result<server::ServiceEvent<FakeTypes>>>;
    type ServiceWriteType = Vec<u8>;
    type ReadResponder = FakeResponder;
    type WriteResponder = FakeResponder;
    type IndicateConfirmationStream = UnboundedReceiver<Result<server::ConfirmationEvent>>;
}

#[derive(Default)]
pub struct FakeCentralInner {
    clients: HashMap<PeerId, FakeClient>,
}

#[derive(Clone)]
pub struct FakeCentral {
    inner: Arc<Mutex<FakeCentralInner>>,
}

impl FakeCentral {
    pub fn new() -> Self {
        Self { inner: Arc::new(Mutex::new(FakeCentralInner::default())) }
    }

    pub fn add_client(&mut self, peer_id: PeerId, client: FakeClient) {
        let _ = self.inner.lock().clients.insert(peer_id, client);
    }
}

impl crate::Central<FakeTypes> for FakeCentral {
    fn scan(&self, _filters: &[crate::central::ScanFilter]) -> ScannedResultStream {
        ScannedResultStream::new()
    }

    fn connect(&self, peer_id: PeerId) -> <FakeTypes as GattTypes>::ConnectFuture {
        let clients = &self.inner.lock().clients;
        let res = match clients.get(&peer_id) {
            Some(client) => Ok(client.clone()),
            None => Err(Error::PeerDisconnected(peer_id)),
        };
        futures::future::ready(res)
    }
}

pub enum FakeServerEvent {
    ReadResponded {
        service_id: server::ServiceId,
        handle: Handle,
        value: Result<Vec<u8>>,
    },
    WriteResponded {
        service_id: server::ServiceId,
        handle: Handle,
        value: Result<()>,
    },
    Notified {
        service_id: server::ServiceId,
        handle: Handle,
        value: Vec<u8>,
        peers: Vec<PeerId>,
    },
    Indicated {
        service_id: server::ServiceId,
        handle: Handle,
        value: Vec<u8>,
        peers: Vec<PeerId>,
        confirmations: UnboundedSender<Result<server::ConfirmationEvent>>,
    },
    Unpublished {
        id: server::ServiceId,
    },
    Published {
        id: server::ServiceId,
        definition: ServiceDefinition,
    },
}

#[derive(Debug)]
struct FakeServerInner {
    services: HashMap<server::ServiceId, ServiceDefinition>,
    service_senders:
        HashMap<server::ServiceId, UnboundedSender<Result<server::ServiceEvent<FakeTypes>>>>,
    sender: UnboundedSender<FakeServerEvent>,
}

#[derive(Debug)]
pub struct FakeServer {
    inner: Arc<Mutex<FakeServerInner>>,
}

impl server::Server<FakeTypes> for FakeServer {
    fn prepare(
        &self,
        service: server::ServiceDefinition,
    ) -> <FakeTypes as ServerTypes>::LocalServiceFut {
        let id = service.id();
        self.inner.lock().services.insert(id, service);
        futures::future::ready(Ok(FakeLocalService::new(id, self.inner.clone())))
    }
}

impl FakeServer {
    pub fn new() -> (Self, UnboundedReceiver<FakeServerEvent>) {
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        (
            Self {
                inner: Arc::new(Mutex::new(FakeServerInner {
                    services: Default::default(),
                    service_senders: Default::default(),
                    sender,
                })),
            },
            receiver,
        )
    }

    pub fn service(&self, id: server::ServiceId) -> Option<ServiceDefinition> {
        self.inner.lock().services.get(&id).cloned()
    }

    pub fn incoming_write(
        &self,
        peer_id: PeerId,
        id: server::ServiceId,
        handle: Handle,
        offset: u32,
        value: Vec<u8>,
    ) {
        // TODO: check that the write is allowed
        let sender = self.inner.lock().sender.clone();
        self.inner
            .lock()
            .service_senders
            .get(&id)
            .unwrap()
            .unbounded_send(Ok(server::ServiceEvent::Write {
                peer_id,
                handle,
                offset,
                value,
                responder: FakeResponder { sender, service_id: id, handle },
            }))
            .unwrap();
    }

    pub fn incoming_read(
        &self,
        peer_id: PeerId,
        id: server::ServiceId,
        handle: Handle,
        offset: u32,
    ) {
        // TODO: check that the read is allowed
        let sender = self.inner.lock().sender.clone();
        self.inner
            .lock()
            .service_senders
            .get(&id)
            .unwrap()
            .unbounded_send(Ok(server::ServiceEvent::Read {
                peer_id,
                handle,
                offset,
                responder: FakeResponder { sender, service_id: id, handle },
            }))
            .unwrap();
    }
}

pub struct FakeLocalService {
    id: server::ServiceId,
    inner: Arc<Mutex<FakeServerInner>>,
}

impl FakeLocalService {
    fn new(id: server::ServiceId, inner: Arc<Mutex<FakeServerInner>>) -> Self {
        Self { id, inner }
    }
}

impl Drop for FakeLocalService {
    fn drop(&mut self) {
        self.inner.lock().services.remove(&self.id);
    }
}

impl LocalService<FakeTypes> for FakeLocalService {
    fn publish(&self) -> <FakeTypes as ServerTypes>::ServiceEventStream {
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        let _ = self.inner.lock().service_senders.insert(self.id, sender);
        let definition = self.inner.lock().services.get(&self.id).unwrap().clone();
        self.inner
            .lock()
            .sender
            .unbounded_send(FakeServerEvent::Published { id: self.id, definition })
            .unwrap();
        receiver
    }

    fn notify(&self, characteristic: &Handle, data: &[u8], peers: &[PeerId]) {
        self.inner
            .lock()
            .sender
            .unbounded_send(FakeServerEvent::Notified {
                service_id: self.id,
                handle: *characteristic,
                value: data.into(),
                peers: peers.into(),
            })
            .unwrap();
    }

    fn indicate(
        &self,
        characteristic: &Handle,
        data: &[u8],
        peers: &[PeerId],
    ) -> <FakeTypes as ServerTypes>::IndicateConfirmationStream {
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        self.inner
            .lock()
            .sender
            .unbounded_send(FakeServerEvent::Indicated {
                service_id: self.id,
                handle: *characteristic,
                value: data.into(),
                peers: peers.into(),
                confirmations: sender,
            })
            .unwrap();
        receiver
    }
}

pub struct FakeResponder {
    sender: UnboundedSender<FakeServerEvent>,
    service_id: server::ServiceId,
    handle: Handle,
}

impl ReadResponder for FakeResponder {
    fn respond(self, value: &[u8]) {
        self.sender
            .unbounded_send(FakeServerEvent::ReadResponded {
                service_id: self.service_id,
                handle: self.handle,
                value: Ok(value.into()),
            })
            .unwrap();
    }

    fn error(self, error: GattError) {
        self.sender
            .unbounded_send(FakeServerEvent::ReadResponded {
                service_id: self.service_id,
                handle: self.handle,
                value: Err(Error::Gatt(error)),
            })
            .unwrap();
    }
}

impl WriteResponder for FakeResponder {
    fn acknowledge(self) {
        self.sender
            .unbounded_send(FakeServerEvent::WriteResponded {
                service_id: self.service_id,
                handle: self.handle,
                value: Ok(()),
            })
            .unwrap();
    }

    fn error(self, error: GattError) {
        self.sender
            .unbounded_send(FakeServerEvent::WriteResponded {
                service_id: self.service_id,
                handle: self.handle,
                value: Err(Error::Gatt(error)),
            })
            .unwrap();
    }
}
