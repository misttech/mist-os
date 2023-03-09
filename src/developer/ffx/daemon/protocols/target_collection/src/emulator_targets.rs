// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{Context, Result};
use emulator_instance::{
    get_all_instances, EmulatorInstanceData, EmulatorInstanceInfo, EngineOption, NetworkingMode,
    EMU_INSTANCE_ROOT_DIR,
};
use ffx::TargetAddrInfo;
use fidl_fuchsia_developer_ffx as ffx;
use fidl_fuchsia_net::{IpAddress, Ipv4Address};
use futures::{
    channel::mpsc::{self, Receiver, Sender},
    stream::StreamExt,
    SinkExt,
};
use notify::{
    event::{
        Event,
        EventKind::{Create, Modify, Remove},
    },
    RecursiveMode, Watcher,
};
use std::{
    collections::HashMap,
    fs::create_dir_all,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
// `RecommendedWatcher` is a type alias to FsEvents in the notify crate.
// On mac this seems to have bugs about what's reported and when regarding
// file removal. Without PollWatcher the watcher would report a fresh file
// as having been deleted even if it is a new file.
//
// See fxbug.dev/114564 for details on what happens when using the Default
// RecommendedWatcher (FsEvents).
//
// It's possible that in future versions of this crate this bug will be fixed,
// so it may be worth revisiting this in the future in order to make the code
// in this file a little cleaner and easier to read.
#[cfg(target_os = "macos")]
use notify::PollWatcher as RecommendedWatcher;
#[cfg(not(target_os = "macos"))]
use notify::RecommendedWatcher;
/// Config key to emulator instance data
#[derive(Debug)]
pub(crate) struct EmulatorWatcher {
    emu_instance_rx: Receiver<EmulatorInstanceEvent>,
    emu_instance_tx: Sender<EmulatorInstanceEvent>,
    // Hold a reference here to the watcher to keep it in scope.
    _watcher: RecommendedWatcher,
}
#[derive(Clone, Debug, PartialEq)]
/// Enum for the payload of emulator instances to check.
/// This is done to allow non-async events from notify::Watcher
/// to be handled as async tasks.
pub(crate) enum EmulatorInstanceEvent {
    Name(String),
    Data(EmulatorInstanceData),
}
/// Action to take for a Target based on an
/// emulator instance. Either Add/Update it
/// or Remove it.
#[derive(Debug, PartialEq)]
pub(crate) enum EmulatorTargetAction {
    Add(ffx::TargetInfo),
    Remove(ffx::TargetInfo),
}
#[derive(Debug)]
/// This struct handles the events from the Watcher.
/// Based on the path modified, the emulator instance is
/// determined and added to the event queue to process.
///
struct EmulatorWatcherHandler {
    /// This is the root of the emulator instances.
    instance_dir: PathBuf,
    /// Sender side to send emulator instances to process.
    emu_instance_tx: mpsc::Sender<EmulatorInstanceEvent>,
    /// Based on the instance name, the writing of events to process
    /// is throttled to send no more often than "cutoff".
    throttle: HashMap<String, Instant>,
    cutoff: Duration,
}
#[tracing::instrument()]
pub(crate) async fn start_emulator_watching() -> Result<EmulatorWatcher> {
    let instance_dir: PathBuf =
        ffx_config::get(EMU_INSTANCE_ROOT_DIR).await.context("Reading emu instance config")?;
    let (emu_instance_tx, emu_instance_rx) = mpsc::channel::<EmulatorInstanceEvent>(100);
    if !instance_dir.exists() {
        create_dir_all(&instance_dir).context("Creating instance root directory")?;
    }
    let watch_handler = EmulatorWatcherHandler {
        instance_dir: instance_dir.clone(),
        emu_instance_tx: emu_instance_tx.clone(),
        cutoff: Duration::from_millis(100),
        throttle: HashMap::new(),
    };
    // Watcher configuration is based on start_socket_watch() in daemon.
    #[cfg(target_os = "macos")]
    let res = RecommendedWatcher::new(
        watch_handler,
        notify::Config::default().with_poll_interval(Duration::from_millis(500)),
    );
    #[cfg(not(target_os = "macos"))]
    let res = RecommendedWatcher::new(watch_handler, notify::Config::default());
    let mut watcher = res.context("Creating emulator watcher")?;
    watcher
        .watch(&instance_dir, RecursiveMode::Recursive)
        .context("Setting emulator watcher context")?;
    let watcher_handler =
        EmulatorWatcher { emu_instance_rx: emu_instance_rx, emu_instance_tx, _watcher: watcher };
    Ok(watcher_handler)
}
impl EmulatorWatcherHandler {
    // Given a PathBuf, return the name of the emulator instance, if any.
    #[tracing::instrument()]
    fn instance_name_from_path<T: AsRef<Path> + std::fmt::Debug>(
        &self,
        instance_path: T,
    ) -> Option<String> {
        let relative =
            instance_path.as_ref().strip_prefix(&self.instance_dir).unwrap_or(Path::new(""));
        let mut name: String = "".into();
        if let Some(instance_name) = relative.parent() {
            name = (&instance_name.to_string_lossy()).to_string();
            if name == "" {
                name = (&relative.to_string_lossy()).to_string();
            }
        } else if !relative.to_string_lossy().is_empty() {
            name = (&relative.to_string_lossy()).to_string();
        }
        if !name.is_empty() {
            Some(name)
        } else {
            None
        }
    }
}
impl notify::EventHandler for EmulatorWatcherHandler {
    fn handle_event(&mut self, event: Result<notify::Event, notify::Error>) {
        match event {
            Ok(Event { kind: Create(_), paths, .. }) | Ok(Event { kind: Modify(_), paths, .. }) => {
                for p in paths {
                    // Filter out tmp files, these have no extension
                    if let Some(_ext) = p.extension() {
                        let now = Instant::now();
                        if let Some(instance_name) = self.instance_name_from_path(&p) {
                            if let Some(last) = self.throttle.get(&instance_name) {
                                if now < *last || now.duration_since(*last) < self.cutoff {
                                    continue;
                                }
                            }
                            self.throttle.insert(instance_name.clone(), now);
                            let _ = self
                                .emu_instance_tx
                                .try_send(EmulatorInstanceEvent::Name(instance_name.clone()))
                                .map_err(|e| {
                                    tracing::error!(
                                        "Error sending emulator instance event: {:?} {e:?}",
                                        &p
                                    )
                                });
                        }
                    }
                }
            }
            Ok(Event { kind: Remove(_), paths, .. }) => {
                for p in paths {
                    if let Some(instance_name) = self.instance_name_from_path(&p) {
                        let _ = self
                            .emu_instance_tx
                            .try_send(EmulatorInstanceEvent::Name(instance_name.clone()))
                            .map_err(|e| {
                                tracing::error!(
                                    "Error sending emulator instance event: {:?} {e:?}",
                                    &p
                                )
                            });
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
            Ok(_) => (),
        }
    }
}
impl EmulatorWatcher {
    /// Returns the action to take with the provided targetInfo for the emulator instance,
    ///  or None if it is not needed.
    pub(crate) async fn emulator_target_detected(&mut self) -> Option<EmulatorTargetAction> {
        if let Some(event) = self.emu_instance_rx.next().await {
            tracing::trace!("checking instance {:?}", event);
            match event {
                EmulatorInstanceEvent::Name(instance_name) => {
                    match emulator_instance::read_from_disk(&instance_name).await {
                        Ok(EngineOption::DoesExist(emu_instance)) => {
                            if let Some(target_info) = Self::handle_instance(&emu_instance) {
                                return Some(EmulatorTargetAction::Add(target_info));
                            } else {
                                return None;
                            }
                        }
                        Ok(EngineOption::DoesNotExist(_)) => {
                            tracing::trace!(
                                "Emulator instance:{:?} does not exist.",
                                &instance_name
                            );
                            return None;
                        }
                        Err(e) => {
                            tracing::trace!("Cannot read emulator instance: {e:?}");
                            // Create a TargetInfo with just the name so it can be passed back
                            // to remove the target.
                            let target_info = ffx::TargetInfo {
                                nodename: Some(instance_name),
                                ..ffx::TargetInfo::EMPTY
                            };
                            return Some(EmulatorTargetAction::Remove(target_info));
                        }
                    }
                }
                EmulatorInstanceEvent::Data(emu_instance) => {
                    if let Some(target_info) = Self::handle_instance(&emu_instance) {
                        return Some(EmulatorTargetAction::Add(target_info));
                    } else {
                        return None;
                    }
                }
            }
        }
        None
    }
    pub(crate) async fn check_all_instances(&mut self) -> Result<()> {
        let instances = get_all_instances().await?;
        for emu in instances {
            self.emu_instance_tx.send(EmulatorInstanceEvent::Data(emu)).await?;
        }
        Ok(())
    }
    fn handle_instance(instance: &EmulatorInstanceData) -> Option<ffx::TargetInfo> {
        if instance.is_running() && instance.get_networking_mode() != &NetworkingMode::Tap {
            tracing::debug!("Making target from {instance:?}");
            Self::make_target(&instance)
        } else {
            None
        }
    }
    #[tracing::instrument()]
    fn make_target(instance: &EmulatorInstanceData) -> Option<ffx::TargetInfo> {
        let nodename: String = instance.get_name().into();
        // TUN/TAP emulators are discoverable via mDNS.
        if instance.get_networking_mode() == &NetworkingMode::Tap {
            tracing::debug!(
                "Skipping making target for {}, since it is tun/tap networking",
                nodename
            );
            return None;
        }
        let ssh_port = instance.get_ssh_port();
        if ssh_port.is_none() {
            // No ssh port assigned so don't create a target.
            tracing::debug!("Skipping making target for {}, since ssh port is none", nodename);
            return None;
        }
        // All emulators run on loopback ipv4.
        let ip = IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] });
        let loopback =
            TargetAddrInfo::IpPort(ffx::TargetIpPort { ip, scope_id: 0, port: ssh_port.unwrap() });
        let ssh_address = Some(loopback.clone());
        if nodename.is_empty() {
            tracing::debug!("Skipping making target for {}, since nodename is empty", nodename);
            return None;
        }
        Some(ffx::TargetInfo {
            nodename: Some(nodename),
            addresses: Some(vec![loopback]),
            ssh_address,
            ..ffx::TargetInfo::EMPTY
        })
    }
}
#[cfg(test)]
mod tests {
    pub(crate) use super::*;
    use emulator_instance::EngineState;
    use notify::{
        event::{CreateKind, EventAttributes, ModifyKind, RemoveKind},
        EventHandler, EventKind,
    };
    use std::process;
    use tempfile::tempdir;
    #[test]
    fn test_instance_name_from_path() -> Result<()> {
        let (emu_instance_tx, _emu_instance_rx) = mpsc::channel::<EmulatorInstanceEvent>(1);
        let temp = tempdir().expect("cannot get tempdir");
        let instance_dir = temp.path().to_path_buf();
        if !instance_dir.exists() {
            create_dir_all(&instance_dir).context("Creating instance root directory")?;
        }
        let watch_handler = EmulatorWatcherHandler {
            instance_dir: instance_dir.clone(),
            emu_instance_tx: emu_instance_tx,
            cutoff: Duration::from_millis(100),
            throttle: HashMap::new(),
        };
        let test_data = vec![
            (instance_dir.join("emu-instance"), Some(String::from("emu-instance"))),
            (PathBuf::from("/someplace/unknown/emu-instance"), None),
            (PathBuf::from("./emu-instance"), None),
            (PathBuf::from("emu-instance"), None),
            (PathBuf::from("emu-instance"), None),
            (PathBuf::from(""), None),
        ];
        for (p, expected) in test_data {
            let actual = watch_handler.instance_name_from_path(&p);
            assert_eq!(actual, expected, "Calling instance_name_from_path({p:?})");
        }
        Ok(())
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_handle_event() -> Result<()> {
        let temp = tempdir().expect("cannot get tempdir");
        let instance_dir = temp.path().to_path_buf();
        if !instance_dir.exists() {
            create_dir_all(&instance_dir).context("Creating instance root directory")?;
        }
        let emu_instance_name = String::from("new-emu-instance");
        let new_instance_dir = instance_dir.join(emu_instance_name.clone());
        let new_instance_engine_file = new_instance_dir.join("engine.json");
        let other_file = new_instance_dir.join("some_other_file.dat");
        // notify events
        let test_events = vec![
            // Creating a directory is not enough, there needs to be files in the directory.
            (
                vec![Event {
                    kind: EventKind::Create(CreateKind::Folder),
                    paths: vec![new_instance_dir.clone()],
                    attrs: EventAttributes::new(),
                }],
                vec![None],
            ),
            // Modify a directory, and creating a json file, should emit an event.
            (
                vec![
                    Event {
                        kind: EventKind::Modify(notify::event::ModifyKind::Any),
                        paths: vec![new_instance_dir.clone()],
                        attrs: EventAttributes::new(),
                    },
                    Event {
                        kind: EventKind::Create(CreateKind::File),
                        paths: vec![new_instance_engine_file.clone()],
                        attrs: EventAttributes::new(),
                    },
                ],
                vec![Some(EmulatorInstanceEvent::Name(emu_instance_name.clone())), None],
            ),
            // Modify a directory, and creating 2 files, should emit just 1 event.
            (
                vec![
                    Event {
                        kind: EventKind::Modify(notify::event::ModifyKind::Any),
                        paths: vec![new_instance_dir.clone()],
                        attrs: EventAttributes::new(),
                    },
                    Event {
                        kind: EventKind::Create(CreateKind::File),
                        paths: vec![new_instance_engine_file.clone(), other_file.clone()],
                        attrs: EventAttributes::new(),
                    },
                ],
                vec![Some(EmulatorInstanceEvent::Name(emu_instance_name.clone())), None],
            ),
            // Modify a directory, and multiple files created 2 files, should emit just 1 event.
            (
                vec![
                    Event {
                        kind: EventKind::Modify(notify::event::ModifyKind::Any),
                        paths: vec![new_instance_dir.clone()],
                        attrs: EventAttributes::new(),
                    },
                    Event {
                        kind: EventKind::Create(CreateKind::File),
                        paths: vec![new_instance_engine_file.clone(), other_file.clone()],
                        attrs: EventAttributes::new(),
                    },
                    Event {
                        kind: EventKind::Modify(ModifyKind::Any),
                        paths: vec![new_instance_engine_file.clone(), other_file.clone()],
                        attrs: EventAttributes::new(),
                    },
                    Event {
                        kind: EventKind::Modify(ModifyKind::Any),
                        paths: vec![new_instance_engine_file.clone(), other_file.clone()],
                        attrs: EventAttributes::new(),
                    },
                ],
                vec![Some(EmulatorInstanceEvent::Name(emu_instance_name.clone())), None],
            ),
            // Modify a directory, and multiple files created 2 files,
            // and then remove everything should emit just 2 events, one for the create/modifies and 1 from the remove
            (
                vec![
                    Event {
                        kind: EventKind::Modify(notify::event::ModifyKind::Any),
                        paths: vec![new_instance_dir.clone()],
                        attrs: EventAttributes::new(),
                    },
                    Event {
                        kind: EventKind::Create(CreateKind::File),
                        paths: vec![new_instance_engine_file.clone(), other_file.clone()],
                        attrs: EventAttributes::new(),
                    },
                    Event {
                        kind: EventKind::Modify(ModifyKind::Any),
                        paths: vec![new_instance_engine_file.clone(), other_file.clone()],
                        attrs: EventAttributes::new(),
                    },
                    Event {
                        kind: EventKind::Remove(RemoveKind::Folder),
                        paths: vec![new_instance_dir.clone()],
                        attrs: EventAttributes::new(),
                    },
                ],
                vec![
                    Some(EmulatorInstanceEvent::Name(emu_instance_name.clone())),
                    Some(EmulatorInstanceEvent::Name(emu_instance_name.clone())),
                    None,
                ],
            ),
        ];
        for (events, expected) in test_events {
            // create a new watcher and channel.
            let (emu_instance_tx, mut emu_instance_rx) = mpsc::channel::<EmulatorInstanceEvent>(10);
            let mut watch_handler = EmulatorWatcherHandler {
                instance_dir: instance_dir.clone(),
                emu_instance_tx,
                cutoff: Duration::from_millis(100),
                throttle: HashMap::new(),
            };
            for event in &events {
                watch_handler.handle_event(Ok(event.clone()));
            }
            let mut actual_events: Vec<Option<EmulatorInstanceEvent>> = vec![];
            loop {
                let actual_event = match emu_instance_rx.try_next() {
                    Ok(emu_event) => emu_event,
                    // try_next Err() means no messages, but the channel is still open.
                    Err(_) => None,
                };
                actual_events.push(actual_event.clone());
                if actual_event.is_none() {
                    break;
                }
            }
            assert_eq!(actual_events, expected, "for events {events:?}");
        }
        Ok(())
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_emulator_target_detected() -> Result<()> {
        let temp = tempdir().expect("cannot get tempdir");
        let instance_dir = temp.path().to_path_buf();
        if !instance_dir.exists() {
            create_dir_all(&instance_dir).context("Creating instance root directory")?;
        }
        let emu_instance_name = String::from("new-emu-instance");
        let new_instance_dir = instance_dir.join(emu_instance_name.clone());
        let _new_instance_engine_file = new_instance_dir.join("engine.json");
        let _other_file = new_instance_dir.join("some_other_file.dat");
        let mut instance_data =
            EmulatorInstanceData::new_with_state("emu-data-instance", EngineState::Running);
        instance_data.set_pid(process::id());
        let mut config = instance_data.get_emulator_configuration_mut();
        config.host.networking = emulator_instance::NetworkingMode::User;
        config.host.port_map.insert(
            String::from("ssh"),
            emulator_instance::PortMapping { guest: 22, host: Some(3322) },
        );
        let mut tap_instance_data = instance_data.clone();
        config = tap_instance_data.get_emulator_configuration_mut();
        config.host.networking = emulator_instance::NetworkingMode::Tap;
        let ip = IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] });
        let loopback = TargetAddrInfo::IpPort(ffx::TargetIpPort { ip, scope_id: 0, port: 3322 });
        let ssh_address = Some(loopback.clone());
        // not running
        // not user mode
        // missing reading
        //returns  Option<(ffx::TargetInfo, bool)> {
        let testdata = vec![
            (
                EmulatorInstanceEvent::Name(emu_instance_name.clone()),
                Some(EmulatorTargetAction::Remove(ffx::TargetInfo {
                    nodename: Some(emu_instance_name.clone()),
                    ..ffx::TargetInfo::EMPTY
                })),
            ),
            (
                EmulatorInstanceEvent::Data(instance_data.clone()),
                Some(EmulatorTargetAction::Add(ffx::TargetInfo {
                    nodename: Some(instance_data.get_name().to_string()),
                    addresses: Some(vec![loopback]),
                    ssh_address,
                    ..ffx::TargetInfo::EMPTY
                })),
            ),
            (EmulatorInstanceEvent::Data(tap_instance_data.clone()), None),
        ];
        for (event, expected) in testdata {
            // create a new watcher and channel.
            let (mut emu_instance_tx, emu_instance_rx) = mpsc::channel::<EmulatorInstanceEvent>(10);
            let watch_handler = EmulatorWatcherHandler {
                instance_dir: instance_dir.clone(),
                emu_instance_tx: emu_instance_tx.clone(),
                cutoff: Duration::from_millis(100),
                throttle: HashMap::new(),
            };
            let iwatcher = RecommendedWatcher::new(
                watch_handler,
                notify::Config::default().with_poll_interval(Duration::from_secs(500 * 60)),
            )?;
            let mut watcher = EmulatorWatcher {
                emu_instance_rx,
                emu_instance_tx: emu_instance_tx.clone(),
                _watcher: iwatcher,
            };
            emu_instance_tx.try_send(event.clone())?;
            let actual = watcher.emulator_target_detected().await;
            assert_eq!(expected, actual, "for event {event:?}");
        }
        Ok(())
    }
}
