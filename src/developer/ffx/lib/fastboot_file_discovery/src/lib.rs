// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use fuchsia_async::Task;
use futures::channel::mpsc::{self, Receiver, Sender};
use futures::StreamExt;
use notify::EventKind::{Create, Modify, Remove};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{create_dir_all, File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::net::{AddrParseError, IpAddr, SocketAddr};
use std::num::ParseIntError;
use std::path::Path;
use std::str::FromStr;
use thiserror::Error;

// Defaults to ${HOME}/.fastboot/devices
pub const FASTBOOT_FILE_PATH: &str = "fastboot.devices_file.path";

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Clone, Copy)]
pub enum FastbootMode {
    TCP,
    UDP,
}

#[derive(Error, Debug, PartialEq)]
pub enum ParseFastbootModeError {
    #[error("Invalid string: {}. Supported: \"tcp\" or \"udp\"", got)]
    InvalidString { got: String },
}

impl FromStr for FastbootMode {
    type Err = ParseFastbootModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tcp" => Ok(Self::TCP),
            "udp" => Ok(Self::UDP),
            e @ _ => Err(ParseFastbootModeError::InvalidString { got: e.to_string() }),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Clone)]
pub struct FastbootEntry {
    mode: FastbootMode,
    port: u16,
    address: IpAddr,
}

impl FastbootEntry {
    pub fn mode(&self) -> FastbootMode {
        self.mode
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn address(&self) -> IpAddr {
        self.address.clone()
    }
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.address, self.port)
    }
}

#[derive(Error, Debug, PartialEq)]
pub enum ParseFastbootEntryError {
    #[error("Invalid format: {}", got)]
    InvalidFormat { got: String },
    #[error("could not parse mode")]
    ModeError(#[from] ParseFastbootModeError),
    #[error("invalid port number")]
    InvalidPortNum(#[from] ParseIntError),
    #[error("invalid ip address number")]
    InvalidAddress(#[from] AddrParseError),
}

impl FromStr for FastbootEntry {
    type Err = ParseFastbootEntryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (connection_type_addr, port_str) = s
            .rsplit_once(':')
            .ok_or(ParseFastbootEntryError::InvalidFormat { got: s.to_string() })?;
        let (connection_type, addr) = connection_type_addr
            .rsplit_once(':')
            .ok_or(ParseFastbootEntryError::InvalidFormat { got: s.to_string() })?;

        let port = port_str.parse::<u16>()?;
        let mode = connection_type.parse::<FastbootMode>()?;

        let address = IpAddr::from_str(addr)?;

        Ok(FastbootEntry { mode, port, address })
    }
}

pub struct FastbootFileWatcher {
    // Task for the drain loop
    drain_task: Option<Task<()>>,
    _watcher: RecommendedWatcher,
}

#[derive(Debug, PartialEq)]
pub enum FastbootEvent {
    Discovered(FastbootEntry),
    Lost(FastbootEntry),
}

#[allow(async_fn_in_trait)]
pub trait FastbootEventHandler: Send + 'static {
    /// Handles an event.
    async fn handle_event(&mut self, event: Result<FastbootEvent>);
}

impl<F> FastbootEventHandler for F
where
    F: FnMut(Result<FastbootEvent>) -> () + Send + 'static,
{
    async fn handle_event(&mut self, x: Result<FastbootEvent>) -> () {
        self(x)
    }
}

pub fn recommended_watcher<F>(
    event_handler: F,
    watch_file: impl AsRef<Path>,
) -> Result<FastbootFileWatcher>
where
    F: FastbootEventHandler,
{
    FastbootFileWatcher::new(event_handler, watch_file)
}

impl FastbootFileWatcher {
    pub fn new<F>(event_handler: F, watch_file_path: impl AsRef<Path>) -> Result<Self>
    where
        F: FastbootEventHandler,
    {
        let (sender, receiver) = mpsc::channel::<FastbootEvent>(100);

        match watch_file_path.as_ref().try_exists() {
            Ok(true) => {}
            _ => {
                // First create the parent in case it doesnt exist
                if let Some(parent) = watch_file_path.as_ref().parent() {
                    create_dir_all(parent)?;
                }
                let _file =
                    OpenOptions::new().write(true).create(true).open(watch_file_path.as_ref());
            }
        }

        let handler =
            FastbootFileHandler { fastboot_file_tx: sender, seen_devices: BTreeSet::new() };
        let mut watcher = RecommendedWatcher::new(handler, Config::default())?;

        watcher.watch(watch_file_path.as_ref(), RecursiveMode::NonRecursive)?;

        let mut res = Self { drain_task: None, _watcher: watcher };

        res.drain_task.replace(Task::local(handle_events_loop(receiver, event_handler)));

        Ok(res)
    }
}

pub fn get_fastboot_devices(device_file_path: &impl AsRef<Path>) -> Result<Vec<FastbootEntry>> {
    match device_file_path.as_ref().try_exists() {
        Ok(false) | Err(_) => {
            return Ok(vec![]);
        }
        Ok(true) => {}
    };
    let file = File::open(&device_file_path)?;
    let lines = BufReader::new(file).lines();
    let mut res = vec![];
    for line in lines.flatten() {
        let device = line.parse::<FastbootEntry>()?;
        res.push(device);
    }
    Ok(res)
}

#[derive(Debug)]
/// This struct handles the events from the Watcher.
struct FastbootFileHandler {
    /// Sender side to send devices to process.
    fastboot_file_tx: Sender<FastbootEvent>,
    seen_devices: BTreeSet<FastbootEntry>,
}

impl notify::EventHandler for FastbootFileHandler {
    fn handle_event(&mut self, event: Result<notify::Event, notify::Error>) {
        match event {
            Ok(Event { kind: Create(_), paths, .. }) | Ok(Event { kind: Modify(_), paths, .. }) => {
                for p in paths {
                    if p.file_name() == Some(OsStr::new("devices")) {
                        // Cool. Open the file, parse and send events

                        tracing::warn!("triggered by {p:?}");
                        match File::open(&p) {
                            Err(e) => {
                                tracing::error!(
                                    "Error opening fastboot devices file: {:?}: {}",
                                    &p,
                                    e
                                );
                            }
                            Ok(file) => {
                                let lines = BufReader::new(file).lines();

                                for line in lines.flatten() {
                                    match line.parse::<FastbootEntry>() {
                                        Err(e) => {
                                            tracing::error!(
                                                "Error parsing fastboot devices file line: {}. {}",
                                                line,
                                                e
                                            );
                                        }
                                        Ok(device) => {
                                            if !self.seen_devices.contains(&device) {
                                                let _ = self
                                                    .fastboot_file_tx
                                                    .try_send(FastbootEvent::Discovered(
                                                        device.clone(),
                                                    ))
                                                    .map_err(|e| {
                                                        tracing::error!(
                                                    "Error sending fastboot event: {:?} {e:?}",
                                                    &p
                                                )
                                                    });
                                                self.seen_devices.insert(device);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(Event { kind: Remove(_), paths, .. }) => {
                for p in paths {
                    tracing::debug!("Removal of {p:?} is being processed");
                    if p.file_name() == Some(OsStr::new("devices")) {
                        for device in &self.seen_devices {
                            let _ = self
                                .fastboot_file_tx
                                .try_send(FastbootEvent::Lost(device.clone()))
                                .map_err(|e| {
                                    tracing::error!("Error sending fastbodt event: {:?} {e:?}", &p)
                                });
                        }
                        self.seen_devices.clear();
                    }
                }
            }
            Err(ref e @ notify::Error { ref kind, .. }) => {
                match kind {
                    notify::ErrorKind::Io(ioe) => {
                        tracing::debug!("IO error. Ignoring {ioe:?}");
                    }
                    _ => {
                        // If we get a non-spurious error, treat that as something that
                        // should cause us to exit.
                        tracing::warn!("Exiting due to file watcher error: {e:?}");
                    }
                }
            }
            Ok(..) => (),
        }
    }
}

async fn handle_events_loop<F>(mut receiver: Receiver<FastbootEvent>, mut handler: F)
where
    F: FastbootEventHandler,
{
    loop {
        let event = receiver.next().await.ok_or_else(|| anyhow!("no event"));
        tracing::trace!("Event loop received event: {:#?}", event);
        handler.handle_event(event).await;
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    // use futures::channel::mpsc::unbounded;
    use pretty_assertions::assert_eq;
    // use std::collections::{HashMap, VecDeque};

    #[fuchsia::test]
    async fn test_parse_fastboot_mode() -> Result<()> {
        let should_be_tcp = "tcp".parse::<FastbootMode>()?;
        assert_eq!(FastbootMode::TCP, should_be_tcp);

        let capitalization_tcp = "tCp".parse::<FastbootMode>();
        assert!(capitalization_tcp.is_err());

        let should_be_udp = "udp".parse::<FastbootMode>()?;
        assert_eq!(FastbootMode::UDP, should_be_udp);

        let capitalization_udp = "UDp".parse::<FastbootMode>();
        assert!(capitalization_udp.is_err());

        assert!("some_string".parse::<FastbootMode>().is_err());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_parse_fastboot_entry() -> Result<()> {
        assert!("tcp:127.0.0.1:81111111111111111111".parse::<FastbootEntry>().is_err());
        assert!("tCp:127.0.0.1:811".parse::<FastbootEntry>().is_err());
        assert!("tCp:totally&not&an&ip&address:811".parse::<FastbootEntry>().is_err());
        assert!("tCp:totally:address:811".parse::<FastbootEntry>().is_err());
        assert!("tCp::811".parse::<FastbootEntry>().is_err());

        assert_eq!(
            FastbootEntry {
                mode: FastbootMode::TCP,
                port: 811,
                address: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            },
            "tcp:127.0.0.1:811".parse::<FastbootEntry>()?
        );

        assert_eq!(
            FastbootEntry {
                mode: FastbootMode::UDP,
                port: 811,
                address: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            },
            "udp:127.0.0.1:811".parse::<FastbootEntry>()?
        );

        Ok(())
    }
}
