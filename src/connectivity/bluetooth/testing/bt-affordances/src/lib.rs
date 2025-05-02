// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use async_utils::hanging_get::client::HangingGetStream;
use fidl_fuchsia_bluetooth::PeerId;
use fidl_fuchsia_bluetooth_bredr::{
    ConnectParameters, L2capParameters, ProfileMarker, ProfileProxy,
};
use fidl_fuchsia_bluetooth_sys::{
    AccessMarker, AccessProxy, HostInfo, HostWatcherMarker, HostWatcherProxy, Peer,
    ProcedureTokenProxy,
};
use fuchsia_async::{LocalExecutor, TimeoutExt};
use fuchsia_bluetooth::types::Channel;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_sync::Mutex;
use futures::channel::{mpsc, oneshot};
use futures::executor::block_on;
use futures::{SinkExt, StreamExt, TryFutureExt};
use std::ffi::{CStr, CString};
use std::sync::LazyLock;
use std::thread;
use zx::sys::zx_status_t;

// TODO(b/414848887): Pass more descriptive errors.
enum Request {
    ReadLocalAddress(oneshot::Sender<Result<[u8; 6], anyhow::Error>>),
    GetPeerId(CString, oneshot::Sender<Result<PeerId, anyhow::Error>>),
    Connect(PeerId, oneshot::Sender<Result<(), anyhow::Error>>),
    Forget(PeerId, oneshot::Sender<Result<(), anyhow::Error>>),
    ConnectL2cap(PeerId, u16, oneshot::Sender<Result<(), anyhow::Error>>),
    SetDiscoverability(bool, oneshot::Sender<Result<(), anyhow::Error>>),
    Stop,
}

struct WorkThread {
    thread_handle: Mutex<Option<thread::JoinHandle<Result<(), anyhow::Error>>>>,
    sender: mpsc::UnboundedSender<Request>,
}

impl WorkThread {
    fn spawn() -> Self {
        let (sender, mut receiver) = mpsc::unbounded::<Request>();
        let thread_handle = thread::spawn(move || {
            LocalExecutor::new().run_singlethreaded(async {
                let mut access_proxy = connect_to_protocol::<AccessMarker>()?;
                let mut profile_proxy = connect_to_protocol::<ProfileMarker>()?;
                let mut host_watcher_stream = HangingGetStream::new_with_fn_ptr(
                    connect_to_protocol::<HostWatcherMarker>()?,
                    HostWatcherProxy::watch,
                );
                let mut peer_watcher_stream = HangingGetStream::new_with_fn_ptr(
                    access_proxy.clone(),
                    AccessProxy::watch_peers,
                );
                let mut host_cache: Vec<HostInfo> = Vec::new();
                let mut peer_cache: Vec<Peer> = Vec::new();
                #[allow(clippy::collection_is_never_read)]
                let mut _l2cap_channel: Option<Channel> = None;
                let mut discoverability_session: Option<ProcedureTokenProxy> = None;

                while let Some(request) = receiver.next().await {
                    match request {
                        Request::ReadLocalAddress(sender) => {
                            sender
                                .send(
                                    get_active_host(&mut host_cache, &mut host_watcher_stream)
                                        .map_ok(|host| {
                                            host.addresses
                                                .clone()
                                                .unwrap()
                                                .first()
                                                .expect("Host has no address")
                                                .bytes
                                        })
                                        .await,
                                )
                                .expect("Failed to send");
                        }
                        Request::GetPeerId(address, result_sender) => {
                            let (_discovery_session, discovery_session_server) =
                                fidl::endpoints::create_proxy();
                            if let Err(err) =
                                access_proxy.start_discovery(discovery_session_server).await?
                            {
                                result_sender
                                    .send(Err(anyhow!(
                                        "fuchsia.bluetooth.sys.Access/StartDiscovery error: {:?}",
                                        err
                                    )))
                                    .expect("Failed to send");
                                continue;
                            }

                            match get_peer(
                                &address,
                                std::time::Duration::from_secs(1),
                                &mut peer_cache,
                                &mut peer_watcher_stream,
                            )
                            .await
                            {
                                Ok(Some(peer)) => {
                                    result_sender
                                        .send(Ok(peer.id.unwrap()))
                                        .expect("Failed to send");
                                }
                                Ok(None) => {
                                    result_sender
                                        .send(Err(anyhow!("Peer not found")))
                                        .expect("Failed to send");
                                }
                                Err(err) => {
                                    result_sender
                                        .send(Err(anyhow!("wait_for_peer() error: {}", err)))
                                        .expect("Failed to send");
                                }
                            }
                        }
                        Request::Forget(peer_id, sender) => {
                            sender
                                .send(forget(&peer_id, &mut access_proxy).await)
                                .expect("Failed to send");
                        }
                        Request::Connect(peer_id, result_sender) => {
                            result_sender
                                .send(connect(&peer_id, &mut access_proxy).await)
                                .expect("Failed to send");
                        }
                        Request::ConnectL2cap(peer_id, psm, result_sender) => {
                            match connect_l2cap(&peer_id, psm, &mut profile_proxy).await {
                                Ok(channel) => {
                                    _l2cap_channel = Some(channel);
                                    result_sender.send(Ok(())).expect("Failed to send");
                                }
                                Err(err) => {
                                    result_sender.send(Err(err)).expect("Failed to send");
                                }
                            }
                        }
                        Request::SetDiscoverability(discoverable, sender) => {
                            if !discoverable {
                                if discoverability_session.take().is_none() {
                                    eprintln!(
                                        "Asked to revoke nonexistent discoverability session."
                                    );
                                }
                                sender.send(Ok(())).expect("Failed to send");
                                continue;
                            }
                            if discoverability_session.is_some() {
                                continue;
                            }
                            let (token, discoverability_session_server) =
                                fidl::endpoints::create_proxy();
                            if let Err(err) = access_proxy
                                .make_discoverable(discoverability_session_server)
                                .await?
                            {
                                sender
                                    .send(Err(anyhow!(
                                        "fuchsia.bluetooth.sys.Access/MakeDiscoverable error: {:?}",
                                        err
                                    )))
                                    .expect("Failed to send");
                                continue;
                            }
                            discoverability_session = Some(token);
                            sender.send(Ok(())).expect("Failed to send");
                        }
                        Request::Stop => {
                            break;
                        }
                    }
                }

                Ok::<(), anyhow::Error>(())
            })?;

            Ok(())
        });

        Self { thread_handle: Mutex::new(Some(thread_handle)), sender }
    }

    fn join(&self) -> zx_status_t {
        block_on(self.sender.clone().send(Request::Stop)).expect("Failed to send");
        if let Err(err) =
            self.thread_handle.lock().take().unwrap().join().expect("Failed to join work thread")
        {
            eprintln!("Work thread exited with error: {}", err);
            return zx::Status::INTERNAL.into_raw();
        }
        zx::Status::OK.into_raw()
    }

    // Write address of active host into `addr_byte_buff`.
    //
    // Returns ZX_ERR_INTERNAL on error (check logs).
    async fn read_local_address(&self, addr_byte_buff: *mut u8) -> Result<(), anyhow::Error> {
        let addr_bytes_slice = unsafe { std::slice::from_raw_parts_mut(addr_byte_buff, 6) };
        let (sender, receiver) = oneshot::channel::<Result<[u8; 6], anyhow::Error>>();
        self.sender.clone().send(Request::ReadLocalAddress(sender)).await.expect("Failed to send");
        addr_bytes_slice.clone_from_slice(&receiver.await.expect("Failed to receive")?);
        Ok(())
    }

    // Get identifier of peer at `address`.
    async fn get_peer_id(&self, address: &CStr) -> Result<PeerId, anyhow::Error> {
        let (sender, receiver) = oneshot::channel::<Result<PeerId, anyhow::Error>>();
        self.sender
            .clone()
            .send(Request::GetPeerId(address.to_owned(), sender))
            .await
            .expect("Failed to send");
        receiver.await.expect("Failed to receive")
    }

    // Connect to peer with given identifier.
    async fn connect_peer(&self, peer_id: PeerId) -> Result<(), anyhow::Error> {
        let (sender, receiver) = oneshot::channel::<Result<(), anyhow::Error>>();
        self.sender.clone().send(Request::Connect(peer_id, sender)).await.expect("Failed to send");
        receiver.await.expect("Failed to receive")
    }

    // Forget peer and delete all bonding information, if peer is found.
    async fn forget_peer(&self, peer_id: PeerId) -> Result<(), anyhow::Error> {
        let (sender, receiver) = oneshot::channel::<Result<(), anyhow::Error>>();
        self.sender.clone().send(Request::Forget(peer_id, sender)).await.expect("Failed to send");
        receiver.await.expect("Failed to receive")
    }

    // Connect a basic L2CAP channel.
    async fn connect_l2cap_channel(&self, peer_id: PeerId, psm: u16) -> Result<(), anyhow::Error> {
        let (sender, receiver) = oneshot::channel::<Result<(), anyhow::Error>>();
        self.sender
            .clone()
            .send(Request::ConnectL2cap(peer_id, psm, sender))
            .await
            .expect("Failed to send");
        receiver.await.expect("Failed to receive")
    }

    // Set discoverability state.
    async fn set_discoverability(&self, discoverable: bool) -> Result<(), anyhow::Error> {
        let (sender, receiver) = oneshot::channel::<Result<(), anyhow::Error>>();
        self.sender
            .clone()
            .send(Request::SetDiscoverability(discoverable, sender))
            .await
            .expect("Failed to send");
        receiver.await.expect("Failed to receive")
    }
}

async fn get_active_host<'a>(
    host_cache: &'a mut Vec<HostInfo>,
    host_watcher_stream: &mut HangingGetStream<HostWatcherProxy, Vec<HostInfo>>,
) -> Result<&'a HostInfo, anyhow::Error> {
    if let Some(host_watcher_result) =
        host_watcher_stream.next().on_timeout(std::time::Duration::from_millis(100), || None).await
    {
        let Ok(new_host_list) = host_watcher_result else {
            return Err(anyhow!(
                "fuchsia.bluetooth.sys.HostWatcher error: {}",
                host_watcher_result.unwrap_err()
            ));
        };
        *host_cache = new_host_list
    }
    host_cache.first().ok_or_else(|| anyhow!("No hosts"))
}

async fn refresh_peer_cache(
    timeout: std::time::Duration,
    peer_cache: &mut Vec<Peer>,
    peer_watcher_stream: &mut HangingGetStream<AccessProxy, (Vec<Peer>, Vec<PeerId>)>,
) -> Result<(), fidl::Error> {
    match peer_watcher_stream.next().on_timeout(timeout, || None).await {
        Some(Ok((updated, removed))) => {
            removed.iter().for_each(|removed_id| {
                let _ = peer_cache.extract_if(.., |peer| peer.id.unwrap() == *removed_id);
            });
            updated.iter().for_each(|updated_peer| {
                let _ =
                    peer_cache.extract_if(.., |peer| peer.id.unwrap() == updated_peer.id.unwrap());
            });
            peer_cache.extend(updated);
            Ok(())
        }
        Some(Err(err)) => Err(err),
        None => Ok(()),
    }
}

// `address` should encode a BD_ADDR as a string of bytes in little-endian order.
// Blocks until peer is discovered if `wait` is set. Otherwise, returns None if peer is not found.
async fn get_peer<'a>(
    address: &CString,
    timeout: std::time::Duration,
    peer_cache: &'a mut Vec<Peer>,
    peer_watcher_stream: &mut HangingGetStream<AccessProxy, (Vec<Peer>, Vec<PeerId>)>,
) -> Result<Option<&'a Peer>, fidl::Error> {
    let addr_matches =
        |peer: &Peer| peer.address.unwrap().bytes.iter().eq(address.to_bytes().iter().rev());
    // To satisfy borrow checker, must first check if peer exists before generating a reference
    // to the peer in the conditional scope. See "Problem case #3" in "non-lexical lifetimes"
    // rust-lang RFC.
    if peer_cache.iter().any(addr_matches) {
        return Ok(Some(peer_cache.iter().find(|peer: &&Peer| addr_matches(peer)).unwrap()));
    }
    refresh_peer_cache(timeout, peer_cache, peer_watcher_stream).await?;
    if peer_cache.iter().any(addr_matches) {
        return Ok(Some(peer_cache.iter().find(|peer: &&Peer| addr_matches(peer)).unwrap()));
    }
    return Ok(None);
}

async fn connect(peer_id: &PeerId, access_proxy: &mut AccessProxy) -> Result<(), anyhow::Error> {
    access_proxy
        .connect(peer_id)
        .await
        .map_err(|fidl_error| anyhow!("fuchsia.bluetooth.sys.Access/Connect error: {}", fidl_error))
        .and_then(|connect_result| {
            connect_result.map_err(|sapphire_err| {
                anyhow!("fuchsia.bluetooth.sys.Access/Connect error: {:?}", sapphire_err)
            })
        })
}

async fn forget(peer_id: &PeerId, access_proxy: &mut AccessProxy) -> Result<(), anyhow::Error> {
    match access_proxy.forget(peer_id).await {
        Err(fidl_error) => Err(anyhow!("fuchsia.bluetooth.sys.Access/Forget error: {fidl_error}")),
        Ok(Err(fidl_fuchsia_bluetooth_sys::Error::PeerNotFound)) => {
            println!("Asked to forget nonexistent peer.");
            Ok(())
        }
        Ok(Err(sapphire_err)) => {
            Err(anyhow!("fuchsia.bluetooth.sys.Access/Forget error: {sapphire_err:?}"))
        }
        Ok(Ok(_)) => Ok(()),
    }
}

async fn connect_l2cap(
    peer_id: &PeerId,
    psm: u16,
    profile_proxy: &mut ProfileProxy,
) -> Result<Channel, anyhow::Error> {
    match profile_proxy
        .connect(
            peer_id,
            &ConnectParameters::L2cap(L2capParameters { psm: Some(psm), ..Default::default() }),
        )
        .await
    {
        Ok(Ok(channel_res)) => Ok(channel_res
            .try_into()
            .map_err(|err| anyhow!("Couldn't convert FIDL to BT channel: {:?}", err))?),
        Ok(Err(sapphire_err)) => {
            Err(anyhow!("fuchsia.bluetooth.bredr.Profile/Connect error: {:?}", sapphire_err))
        }
        Err(fidl_err) => {
            Err(anyhow!("fuchsia.bluetooth.bredr.Profile/Connect error: {}", fidl_err))
        }
    }
}

static WORKER: LazyLock<WorkThread> = LazyLock::new(|| WorkThread::spawn());

/// Stop serving Rust affordances.
///
/// Returns ZX_STATUS_BAD_STATE if Rust affordances are not running.
/// Returns ZX_STATUS_INTERNAL if Rust affordances exited with an error (check logs).
#[no_mangle]
pub extern "C" fn stop_rust_affordances() -> zx_status_t {
    println!("Stopping Rust affordances");
    WORKER.join()
}

/// Populates `addr_byte_buff` with public address of active host.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
///
/// # Safety
///
/// The caller must ensure that `addr_byte_buff` points to a valid buffer of 6 bytes.
#[no_mangle]
pub extern "C" fn read_local_address(addr_byte_buff: *mut u8) -> zx_status_t {
    if let Err(err) = block_on(WORKER.read_local_address(addr_byte_buff)) {
        eprintln!("read_local_address encountered error: {}", err);
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

/// Get identifier of peer with given `address`.
///
/// Returns 0 on error.
///
/// # Safety
///
/// The caller must ensure that `address` points to a valid C string encoding a BD_ADDR as a string
/// of bytes in little-endian order.
#[no_mangle]
pub unsafe extern "C" fn get_peer_id(address: *const core::ffi::c_char) -> u64 {
    let address = CStr::from_ptr(address);
    match block_on(WORKER.get_peer_id(address)) {
        Ok(peer_id) => peer_id.value,
        Err(err) => {
            eprintln!("connect_peer encountered error: {}", err);
            0
        }
    }
}

/// Connect to peer with given identifier.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
#[no_mangle]
pub extern "C" fn connect_peer(peer_id: u64) -> zx_status_t {
    let peer_id = PeerId { value: peer_id };

    if let Err(err) = block_on(WORKER.connect_peer(peer_id)) {
        eprintln!("connect_peer encountered error: {}", err);
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

/// Remove all bonding information and disconnect peer with given identifier, if found.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
#[no_mangle]
pub extern "C" fn forget_peer(peer_id: u64) -> zx_status_t {
    let peer_id = PeerId { value: peer_id };

    if let Err(err) = block_on(WORKER.forget_peer(peer_id)) {
        eprintln!("forget_peer encountered error: {:?}", err);
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

/// Connect an L2CAP channel on a specific PSM to an already-connected peer. Calling this again will
/// result in the channel being closed after the new channel is opened.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
#[no_mangle]
pub extern "C" fn connect_l2cap_channel(peer_id: u64, psm: u16) -> zx_status_t {
    let peer_id = PeerId { value: peer_id };

    if let Err(err) = block_on(WORKER.connect_l2cap_channel(peer_id, psm)) {
        eprintln!("connect_l2cap_channel encountered error: {:?}", err);
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

/// Start or revoke discoverability.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
#[no_mangle]
pub extern "C" fn set_discoverability(discoverable: bool) -> zx_status_t {
    if let Err(err) = block_on(WORKER.set_discoverability(discoverable)) {
        eprintln!("set_discoverability encountered error: {:?}", err);
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}
