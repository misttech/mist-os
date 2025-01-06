// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "256"]

use anyhow::{format_err, Context as _, Error};
use argh::FromArgs;
use fidl::Error as FidlError;
use fidl_fuchsia_bluetooth_snoop::{
    CaptureError, DevicePackets, SnoopPacket as FidlSnoopPacket, SnoopRequest, SnoopRequestStream,
    SnoopStartRequest, UnrecognizedDeviceName,
};
use fidl_fuchsia_io::DirectoryProxy;
use fuchsia_component::server::ServiceFs;
use fuchsia_fs::directory::{WatchEvent, WatchMessage, Watcher};
use futures::future::{join, ready, Join, Ready};
use futures::select;
use futures::stream::{FusedStream, FuturesUnordered, Stream, StreamExt, StreamFuture};
use log::{debug, error, info, trace, warn};
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;
use {fidl_fuchsia_io as fio, fuchsia_inspect as inspect, fuchsia_trace as trace};

use crate::packet_logs::PacketLogs;
use crate::snooper::{SnoopPacket, Snooper};
use crate::subscription_manager::SubscriptionManager;

mod bounded_queue;
mod packet_logs;
mod snooper;
mod subscription_manager;
#[cfg(test)]
mod tests;

/// Root directory of all HCI devices
const HCI_DEVICE_CLASS_PATH: &str = "/dev/class/bt-hci";

/// A `DeviceId` represents the name of a host device within the HCI_DEVICE_CLASS_PATH.
pub(crate) type DeviceId = String;

/// A request is a tuple of the client id, and the next request or error from the stream, or None
/// if the stream has closed.
type ClientRequest = (ClientId, Option<Result<SnoopRequest, FidlError>>);

/// A `Stream` that holds a collection of client request streams and will return the item from the
/// next ready stream.
type ConcurrentClientRequestFutures =
    FuturesUnordered<Join<Ready<ClientId>, StreamFuture<SnoopRequestStream>>>;

/// A `Stream` that holds a collection of snooper streams and will return the item from the
/// next ready stream.
type ConcurrentSnooperPacketFutures = FuturesUnordered<StreamFuture<Snooper>>;

/// A `ClientId` represents the unique identifier for a client that has connected to the bt-snoop
/// service.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ClientId(u64);

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Generates 64-bit ids in increasing order with wrap around behavior at `u64::MAX`
/// Ids will be unique, as long as there is not a client that lives longer than the
/// next 2^63-1 clients.
struct IdGenerator(ClientId);

impl IdGenerator {
    fn new() -> IdGenerator {
        IdGenerator(ClientId(0))
    }
    fn next(&mut self) -> ClientId {
        let id = self.0;
        (self.0).0 = (self.0).0.wrapping_add(1);
        id
    }
}

/// Handle an event on the virtual filesystem in the HCI device directory.
fn handle_hci_device_event(
    message: WatchMessage,
    directory: &DirectoryProxy,
    snoopers: &mut ConcurrentSnooperPacketFutures,
    subscribers: &mut SubscriptionManager,
    packet_logs: &mut PacketLogs,
) {
    let WatchMessage { event, filename } = message;

    let path = filename.to_str().expect("utf-8 path");
    match event {
        WatchEvent::ADD_FILE | WatchEvent::EXISTING => {
            if filename == std::path::Path::new(".") {
                return;
            }
            info!(path; "Opening snoop channel");
            match Snooper::new(directory, &path) {
                Ok(snooper) => {
                    snoopers.push(snooper.into_future());
                    let removed_device = packet_logs.add_device(path.to_owned());
                    if let Some(device) = removed_device {
                        subscribers.remove_device(&device);
                    }
                }
                Err(e) => {
                    warn!("Failed to open snoop channel for \"{path}\": {e:?}");
                }
            }
        }
        WatchEvent::REMOVE_FILE => {
            info!("Removing snoop channel for hci device: \"{path}\"");
            // TODO(https://fxbug.dev/319447676):
            // What should be done with the logged packets in this case?
            // Find out how to remove snooper from ConcurrentTask (perhaps cancel and wake)
            // Can possibly reopen device logs for devices that are on disk that were evicted from
            // the packet logs collection in the past.
        }
        _ => (),
    }
}

fn register_new_client(
    stream: SnoopRequestStream,
    client_stream: &mut ConcurrentClientRequestFutures,
    client_id: ClientId,
) {
    client_stream.push(join(ready(client_id), stream.into_future()));
}

/// Handle a client request to dump the packet log, subscribe to future events or do both.
/// Returns an error if the client channel does not accept a response that it requested, or a
/// boolean indicating if the client should receive ongoing packets.
async fn handle_client_request(
    request: ClientRequest,
    subscribers: &mut SubscriptionManager,
    packet_logs: &PacketLogs,
) -> Result<bool, Error> {
    let (id, request) = request;
    info!("Request received from client {id}.");
    match request {
        Some(Ok(SnoopRequest::Start {
            payload: SnoopStartRequest { follow, host_device, client, .. },
            ..
        })) => {
            info!("Start request from client: {follow:?}, {host_device:?}");

            let Some(client) = client.map(|client| client.into_proxy()) else {
                warn!("No client delivered, skipping");
                return Ok(true);
            };

            let device_ids: Vec<String> = match &host_device {
                Some(device) => {
                    let Some(_log) = packet_logs.get(device) else {
                        warn!("Couldn't find device: {device}, sending error to client");
                        let _ = client.error(&CaptureError::UnrecognizedDeviceName(
                            UnrecognizedDeviceName::default(),
                        ));
                        drop(client);
                        return Ok(true);
                    };
                    vec![device.clone()]
                }
                None => packet_logs.device_ids().cloned().collect(),
            };

            let mut dev_packets: HashMap<_, _> = Default::default();
            for device in &device_ids {
                let log = packet_logs.get(device).unwrap();
                let packets: &mut Vec<FidlSnoopPacket> =
                    dev_packets.entry(device.clone()).or_insert_with(Vec::new);
                packets.extend(log.lock().iter_mut().map(|e| (&*e).to_fidl()));
            }

            for (device, packets) in dev_packets.into_iter() {
                if packets.len() == 0 {
                    continue;
                }
                if let Err(e) = client
                    .observe(&DevicePackets {
                        host_device: Some(device),
                        packets: Some(packets),
                        ..Default::default()
                    })
                    .await
                {
                    warn!("Failed to send a previously observed packet to client: {e:?}");
                    return Ok(true);
                }
            }

            if follow.is_some_and(|f| f) {
                if let Err(e) = subscribers.register(id, client, host_device) {
                    warn!("Failed to register new subscriber: {e:?}");
                }
            }
        }
        Some(Ok(_)) => {
            warn!("Unknown method called on Snoop from {id:?}, closing stream");
        }
        Some(Err(e)) => {
            warn!("Client returned error: {e:?}");
            subscribers.deregister(&id);
        }
        None => {
            debug!("Client disconnected");
            subscribers.deregister(&id);
        }
    }
    Ok(false)
}

/// Handle a possible incoming packet. Returns an error if the snoop channel is closed and cannot
/// be reopened.
fn handle_packet(
    packet: Option<(DeviceId, SnoopPacket)>,
    snooper: Snooper,
    snoopers: &mut ConcurrentSnooperPacketFutures,
    subscribers: &mut SubscriptionManager,
    packet_logs: &mut PacketLogs,
    truncate_payload: Option<usize>,
) {
    let Some((device, mut packet)) = packet else {
        info!("Snoop channel closed for device: {}", snooper.device_name);
        return;
    };
    trace!("Received packet from {}.", snooper.device_name);
    if let Some(len) = truncate_payload {
        packet.payload.truncate(len);
    }
    subscribers.notify(&device, &packet);
    packet_logs.log_packet(&device, packet);
    snoopers.push(snooper.into_future());
}

struct SnoopConfig {
    log_size_soft_max_bytes: usize,
    log_size_hard_max_bytes: usize,
    log_time: Duration,
    max_device_count: usize,
    truncate_payload: Option<usize>,

    // Inspect tree
    _config_inspect: inspect::Node,
    _log_size_soft_max_bytes_property: inspect::UintProperty,
    _log_size_hard_max_bytes_property: inspect::StringProperty,
    _log_time_property: inspect::UintProperty,
    _max_device_count_property: inspect::UintProperty,
    _truncate_payload_property: inspect::StringProperty,
    _hci_dir_property: inspect::StringProperty,
}

impl SnoopConfig {
    /// Creates a strongly typed `SnoopConfig` out of primitives parsed from the command line
    fn from_args(args: Args, config_inspect: inspect::Node) -> SnoopConfig {
        let log_size_soft_max_bytes = args.log_size_soft_kib * 1024;
        let log_size_hard_max_bytes = args.log_size_hard_kib * 1024;
        let log_time = Duration::from_secs(args.log_time_seconds);
        let _log_size_soft_max_bytes_property =
            config_inspect.create_uint("log_size_soft_max_bytes", log_size_soft_max_bytes as u64);
        let hard_max = if log_size_hard_max_bytes == 0 {
            "No Hard Max".to_string()
        } else {
            log_size_hard_max_bytes.to_string()
        };
        let _log_size_hard_max_bytes_property =
            config_inspect.create_string("log_size_hard_max_bytes", &hard_max);
        let _log_time_property = config_inspect.create_uint("log_time", log_time.as_secs());
        let _max_device_count_property =
            config_inspect.create_uint("max_device_count", args.max_device_count as u64);
        let truncate = args
            .truncate_payload
            .as_ref()
            .map(|n| format!("{} bytes", n))
            .unwrap_or_else(|| "No Truncation".to_string());
        let _truncate_payload_property =
            config_inspect.create_string("truncate_payload", &truncate);
        let _hci_dir_property = config_inspect.create_string("hci_dir", HCI_DEVICE_CLASS_PATH);

        SnoopConfig {
            log_size_soft_max_bytes,
            log_size_hard_max_bytes,
            log_time,
            max_device_count: args.max_device_count,
            truncate_payload: args.truncate_payload,
            _config_inspect: config_inspect,
            _log_size_soft_max_bytes_property,
            _log_size_hard_max_bytes_property,
            _log_time_property,
            _max_device_count_property,
            _truncate_payload_property,
            _hci_dir_property,
        }
    }
}

#[derive(FromArgs)]
/// Log bluetooth snoop packets and provide them to clients.
struct Args {
    #[argh(option, default = "32")]
    /// packet storage buffer size after which packets will start aging off.
    log_size_soft_kib: usize,
    #[argh(option, default = "256")]
    /// hard maximum size in KiB of the buffer to store packets in.
    /// a value of "0" indicates no limit. Defaults to 0.
    log_size_hard_kib: usize,
    #[argh(option, default = "60")]
    /// minimum time to store packets in a snoop log in seconds.
    log_time_seconds: u64,
    #[argh(option, default = "8")]
    /// maximum number of devices for which to store logs.
    max_device_count: usize,
    #[argh(option)]
    /// maximum number of bytes to keep in the payload of incoming packets. Defaults to no limit.
    truncate_payload: Option<usize>,
}

/// Setup the main loop of execution in a Task and run it.
async fn run(
    config: SnoopConfig,
    mut service_handler: impl Unpin + FusedStream + Stream<Item = SnoopRequestStream>,
    inspect: inspect::Node,
) -> Result<(), Error> {
    let mut id_gen = IdGenerator::new();
    let directory =
        fuchsia_fs::directory::open_in_namespace(HCI_DEVICE_CLASS_PATH, fio::Flags::empty())
            .expect("Failed to open hci dev directory");
    let mut hci_device_events =
        Watcher::new(&directory).await.context("Cannot create device watcher")?;
    let mut client_requests = ConcurrentClientRequestFutures::new();
    let mut subscribers = SubscriptionManager::new();
    let mut snoopers = ConcurrentSnooperPacketFutures::new();
    let mut packet_logs = PacketLogs::new(
        config.max_device_count,
        config.log_size_soft_max_bytes,
        config.log_size_hard_max_bytes,
        config.log_time,
        inspect,
    );

    debug!("Capturing snoop packets...");

    loop {
        select! {
            // A new client has connected to one of the exposed services.
            request_stream = service_handler.select_next_some() => {
                let client_id = id_gen.next();
                info!("New client connection: {client_id}");
                register_new_client(request_stream, &mut client_requests, client_id);
            },

            // A new filesystem event in the hci device watch directory has been received.
            event = hci_device_events.next() => {
                let message = event
                    .ok_or_else(|| format_err!("Cannot reach watch server"))
                    .and_then(|r| Ok(r?));
                match message {
                    Ok(message) => {
                        handle_hci_device_event(message, &directory, &mut snoopers, &mut subscribers,
                            &mut packet_logs);
                    }
                    Err(e) => {
                        // Attempt to recreate watcher in the event of an error.
                        warn!("VFS Watcher has died with error: {:?}", e);
                        hci_device_events = Watcher::new(&directory).await
                            .context("Cannot create device watcher")?;
                    }
                }
            },

            // A client has made a request to the server.
            request = client_requests.select_next_some() => {
                let (client_id, (request, client_stream)) = request;
                match handle_client_request((client_id, request),
                    &mut subscribers, &packet_logs).await {
                 Err(e) => {
                    warn!("Error handling client request: {e:?}");
                 },
                 Ok(true) => register_new_client(client_stream, &mut client_requests, client_id),
                 _ => {},
                }
            },

            // A new snoop packet has been received from an hci device.
            (packet, snooper) = snoopers.select_next_some() => {
                trace::duration!(c"bluetooth", c"Snoop::ProcessPacket");
                handle_packet(packet, snooper, &mut snoopers, &mut subscribers,
                    &mut packet_logs, config.truncate_payload);
            },
        }
    }
}

/// Parse program arguments, call the main loop, and log any unrecoverable errors.
/// TODO(https://fxbug.dev/42076557): migrate runtime config to structured config.
#[fuchsia::main(logging_tags=["bt-snoop"])]
async fn main() {
    let args: Args = argh::from_env();

    let mut fs = ServiceFs::new();

    let inspector = inspect::Inspector::default();
    let _inspect_server_task =
        inspect_runtime::publish(&inspector, inspect_runtime::PublishOptions::default());

    let config_inspect = inspector.root().create_child("configuration");
    let runtime_inspect = inspector.root().create_child("runtime_metrics");

    let config = SnoopConfig::from_args(args, config_inspect);

    let _ = fs.dir("svc").add_fidl_service(|stream: SnoopRequestStream| stream);

    let _ = fs.take_and_serve_directory_handle().expect("serve ServiceFS directory");

    match run(config, fs.fuse(), runtime_inspect).await {
        Err(err) => error!("Failed with critical error: {:?}", err),
        _ => {}
    };
}
