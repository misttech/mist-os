// Copyright 2021 The Fuchsia Authors. All rights 1eserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::emulator_watcher::EmulatorWatcher;
pub use crate::events::*;
use crate::fastboot_file_watcher::FastbootWatcher;
use anyhow::{anyhow, Result};
use bitflags::bitflags;
use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use futures::Stream;
use manual_targets::watcher::{
    recommended_watcher as manual_recommended_watcher, ManualTargetEvent, ManualTargetWatcher,
};
use mdns_discovery::{recommended_watcher, MdnsWatcher};
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use usb_fastboot_discovery::{
    recommended_watcher as fastboot_watcher, FastbootEvent, FastbootUsbWatcher,
};
// TODO(colnnelson): Long term it would be nice to have this be pulled into the mDNS library
// so that it can speak our language. Or even have the mdns library not export FIDL structs
// but rather some other well-defined type
use fidl_fuchsia_developer_ffx as ffx;

pub mod desc;
mod emulator_watcher;
pub mod events;
mod fastboot_file_watcher;
pub mod query;

#[allow(dead_code)]
/// A stream of new devices as they appear on the bus. See [`wait_for_devices`].
pub struct TargetStream {
    filter: Option<Box<dyn TargetFilter>>,

    /// Watches mdns events
    mdns_watcher: Option<MdnsWatcher>,

    /// Watches for FastbootUsb events
    fastboot_usb_watcher: Option<FastbootUsbWatcher>,

    /// Watches for ManualTarget events
    manual_targets_watcher: Option<ManualTargetWatcher>,

    /// Watches for Emulator events
    emulator_watcher: Option<EmulatorWatcher>,

    /// Watches for Emulator events
    fastboot_file_watcher: Option<FastbootWatcher>,

    /// This is where results from the various watchers are published.
    queue: UnboundedReceiver<Result<TargetEvent>>,

    /// Whether we want to get Added events.
    notify_added: bool,

    /// Whether we want to get Removed events.
    notify_removed: bool,
}

pub trait TargetFilter: Send + 'static {
    fn filter_target(&mut self, handle: &TargetHandle) -> bool;
}

impl<F> TargetFilter for F
where
    F: FnMut(&TargetHandle) -> bool + Send + 'static,
{
    fn filter_target(&mut self, handle: &TargetHandle) -> bool {
        self(handle)
    }
}

pub trait TargetEventStream: Stream<Item = Result<TargetEvent>> + std::marker::Unpin {}

impl TargetEventStream for TargetStream {}

pub trait TargetDiscovery<F> {
    fn discover_devices(
        &self,
        filter: F,
    ) -> impl std::future::Future<Output = Result<impl TargetEventStream>>;
}

pub struct DiscoveryBuilder {
    emulator_instance_root: Option<PathBuf>,
    fastboot_devices_file_path: Option<PathBuf>,
    notify_added: bool,
    notify_removed: bool,
    sources: DiscoverySources,
}

impl DiscoveryBuilder {
    pub fn set_source(mut self, source: DiscoverySources) -> Self {
        self.sources = source;
        self
    }

    pub fn with_source(mut self, source: DiscoverySources) -> Self {
        self.sources.insert(source);
        self
    }

    pub fn with_emulator_instance_root(mut self, emulator_instance_root: PathBuf) -> Self {
        self.emulator_instance_root = Some(emulator_instance_root);
        self.sources.insert(DiscoverySources::EMULATOR);
        self
    }

    pub fn with_fastboot_devices_file_path(mut self, fastboot_devices_file_path: PathBuf) -> Self {
        self.fastboot_devices_file_path = Some(fastboot_devices_file_path);
        self.sources.insert(DiscoverySources::FASTBOOT_FILE);
        self
    }

    pub fn notify_added(mut self, notify_added: bool) -> Self {
        self.notify_added = notify_added;
        self
    }

    pub fn notify_removed(mut self, notify_removed: bool) -> Self {
        self.notify_removed = notify_removed;
        self
    }

    pub fn build(self) -> Discovery {
        Discovery {
            emulator_instance_root: self.emulator_instance_root,
            fastboot_devices_file_path: self.fastboot_devices_file_path,
            notify_added: self.notify_added,
            notify_removed: self.notify_removed,
            sources: self.sources,
        }
    }
}

impl Default for DiscoveryBuilder {
    fn default() -> Self {
        Self {
            emulator_instance_root: None,
            fastboot_devices_file_path: None,
            notify_added: true,
            notify_removed: true,
            sources: DiscoverySources::default(),
        }
    }
}

pub struct Discovery {
    emulator_instance_root: Option<PathBuf>,
    fastboot_devices_file_path: Option<PathBuf>,
    notify_added: bool,
    notify_removed: bool,
    sources: DiscoverySources,
}

impl Discovery {
    pub fn builder() -> DiscoveryBuilder {
        DiscoveryBuilder::default()
    }
}

impl<F> TargetDiscovery<F> for Discovery
where
    F: TargetFilter,
{
    #[allow(refining_impl_trait)]
    async fn discover_devices(&self, filter: F) -> Result<TargetStream> {
        let stream = wait_for_devices(
            filter,
            self.emulator_instance_root.clone(),
            self.fastboot_devices_file_path.clone(),
            self.notify_added,
            self.notify_removed,
            self.sources,
        )
        .await?;
        Ok(stream)
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DiscoverySources: u8 {
        const MDNS = 1 << 0;
        const USB = 1 << 1;
        const MANUAL = 1 << 2;
        const EMULATOR = 1 << 3;
        const FASTBOOT_FILE = 1 << 4;
    }
}

impl Default for DiscoverySources {
    fn default() -> Self {
        DiscoverySources::all()
    }
}

pub async fn wait_for_devices<F>(
    filter: F,
    emulator_instance_root: Option<PathBuf>,
    fastboot_devices_file_path: Option<PathBuf>,
    notify_added: bool,
    notify_removed: bool,
    sources: DiscoverySources,
) -> Result<TargetStream>
where
    F: TargetFilter,
{
    let (sender, queue) = unbounded();
    // MDNS Watcher
    let mdns_watcher = if sources.contains(DiscoverySources::MDNS) {
        let mdns_sender = sender.clone();
        Some(recommended_watcher(move |res: Result<ffx::MdnsEventType>| {
            // Translate the result to a TargetEvent
            let event = match res {
                Ok(r) => target_event_from_mdns_event(r),
                Err(e) => Some(Err(anyhow!(e))),
            };
            if let Some(event) = event {
                let _ = mdns_sender.unbounded_send(event);
            }
        })?)
    } else {
        None
    };

    // USB Fastboot watcher
    let fastboot_usb_watcher = if sources.contains(DiscoverySources::USB) {
        let fastboot_sender = sender.clone();
        Some(fastboot_watcher(move |res: Result<FastbootEvent>| {
            // Translate the result to a TargetEvent
            tracing::debug!("discovery watcher got fastboot event: {:#?}", res);
            let event = match res {
                Ok(r) => {
                    let event: TargetEvent = r.into();
                    Ok(event)
                }
                Err(e) => Err(anyhow!(e)),
            };
            let _ = fastboot_sender.unbounded_send(event);
        })?)
    } else {
        None
    };

    // Manual Targets watcher
    let manual_targets_watcher = if sources.contains(DiscoverySources::MANUAL) {
        let manual_targets_sender = sender.clone();
        Some(manual_recommended_watcher(move |res: Result<ManualTargetEvent>| {
            // Translate the result to a TargetEvent
            tracing::trace!("discovery watcher got manual target event: {:#?}", res);
            let event = match res {
                Ok(r) => {
                    let event: TargetEvent = r.into();
                    Ok(event)
                }
                Err(e) => Err(anyhow!(e)),
            };
            let _ = manual_targets_sender.unbounded_send(event);
        })?)
    } else {
        None
    };

    let emulator_watcher = if sources.contains(DiscoverySources::EMULATOR) {
        let emulator_sender = sender.clone();
        if let Some(instance_root) = emulator_instance_root {
            Some(EmulatorWatcher::new(instance_root, emulator_sender)?)
        } else {
            None
        }
    } else {
        None
    };

    let fastboot_file_watcher = if sources.contains(DiscoverySources::FASTBOOT_FILE) {
        let fastboot_file_sender = sender;
        if let Some(fastboot_devices_file) = fastboot_devices_file_path {
            Some(FastbootWatcher::new(fastboot_devices_file, fastboot_file_sender)?)
        } else {
            None
        }
    } else {
        None
    };

    Ok(TargetStream {
        filter: Some(Box::new(filter)),
        queue,
        notify_added,
        notify_removed,
        mdns_watcher,
        fastboot_usb_watcher,
        manual_targets_watcher,
        emulator_watcher,
        fastboot_file_watcher,
    })
}

impl Stream for TargetStream {
    type Item = Result<TargetEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(event) = ready!(Pin::new(&mut self.queue).poll_next(cx)) else {
            return Poll::Ready(None);
        };

        let event = event?;

        if let Some(ref mut filter) = self.filter {
            // TODO(colnnelson): This destructure feels odd. Can this be done
            // better or differently?
            let handle = match event {
                TargetEvent::Added(ref handle) => handle,
                TargetEvent::Removed(ref handle) => handle,
            };

            if !filter.filter_target(handle) {
                tracing::trace!(
                    "Skipping event for target handle: {} as it did not match our filter",
                    handle
                );
                // Schedule the future for this to be woken up again.
                cx.waker().clone().wake();
                return Poll::Pending;
            }
        }

        if matches!(event, TargetEvent::Added(_)) && self.notify_added
            || matches!(event, TargetEvent::Removed(_)) && self.notify_removed
        {
            return Poll::Ready(Some(Ok(event)));
        }

        // Schedule the future for this to be woken up again.
        cx.waker().clone().wake();
        return Poll::Pending;
    }
}

#[cfg(test)]
pub mod test {
    use super::*;
    use addr::TargetAddr;
    use futures::StreamExt;
    use pretty_assertions::assert_eq;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::fs::File;
    use std::io::Write;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::rc::Rc;
    use std::str::FromStr;

    /// Used for testing functions that take a TargetDiscovery<TargetFilter>
    ///
    /// In discover_devices the `filter` parameter is explicitly not used.
    /// Test authors are expected to pass the VecDeque with the events "pre filtered"
    /// You should have separate tests for your TargetFilter impls
    pub struct TestDiscovery {
        events: Rc<RefCell<VecDeque<Result<TargetEvent>>>>,
    }

    impl<F> TargetDiscovery<F> for TestDiscovery
    where
        F: TargetFilter,
    {
        #[allow(refining_impl_trait)]
        async fn discover_devices(&self, _filter: F) -> Result<TestTargetStream> {
            Ok(TestTargetStream { events: self.events.clone() })
        }
    }

    pub struct TestTargetStream {
        events: Rc<RefCell<VecDeque<Result<TargetEvent>>>>,
    }

    impl TargetEventStream for TestTargetStream {}

    impl Stream for TestTargetStream {
        type Item = Result<TargetEvent>;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let event = self.events.borrow_mut().pop_front();
            Poll::Ready(event)
        }
    }

    ///////////////////////////////////////////////////////////////////////////
    // Example TestDiscovery Usage
    ///////////////////////////////////////////////////////////////////////////

    fn write_target_event<W: Write>(writer: &mut W, event: TargetEvent) -> Result<()> {
        let symbol = match event {
            TargetEvent::Added(_) => "+",
            TargetEvent::Removed(_) => "-",
        };

        let handle = match event {
            TargetEvent::Added(handle) => handle,
            TargetEvent::Removed(handle) => handle,
        };

        let node_name = handle.node_name.unwrap_or("<unknown>".to_string());
        let state = handle.state;

        writeln!(writer, "{symbol}  {node_name}  {state}")?;
        Ok(())
    }

    /// Writes the events in a target event stream to the given writer
    async fn write_event_stream(writer: &mut impl Write, mut stream: impl TargetEventStream) {
        while let Some(s) = stream.next().await {
            if let Ok(event) = s {
                let _ = write_target_event(writer, event);
            }
        }
    }

    #[fuchsia::test]
    async fn test_write_event_stream() -> Result<()> {
        let mut writer = vec![];
        let disco = TestDiscovery {
            events: Rc::new(RefCell::new(VecDeque::from([
                Ok(TargetEvent::Added(TargetHandle {
                    node_name: Some("magnus".to_string()),
                    state: TargetState::Unknown,
                })),
                Ok(TargetEvent::Added(TargetHandle {
                    node_name: Some("abagail".to_string()),
                    state: TargetState::Unknown,
                })),
                Ok(TargetEvent::Removed(TargetHandle {
                    node_name: Some("abagail".to_string()),
                    state: TargetState::Unknown,
                })),
            ]))),
        };

        let stream = disco.discover_devices(|_: &_| true).await?;

        write_event_stream(&mut writer, stream).await;

        assert_eq!(
            String::from_utf8(writer).expect("we write uft8"),
            r#"+  magnus  Unknown
+  abagail  Unknown
-  abagail  Unknown
"#
        );

        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    // Test DiscoveryBuilder
    ///////////////////////////////////////////////////////////////////////////

    #[test]
    fn test_discovery_builder_default() -> Result<()> {
        let discovery = Discovery::builder().build();
        assert_eq!(discovery.notify_added, true);
        assert_eq!(discovery.notify_removed, true);
        assert_eq!(discovery.sources, DiscoverySources::all());
        assert!(discovery.emulator_instance_root.is_none());
        Ok(())
    }

    #[test]
    fn test_discovery_builder_changes() -> Result<()> {
        let discovery = Discovery::builder()
            .notify_added(false)
            .notify_removed(false)
            .set_source(DiscoverySources::MANUAL)
            .with_source(DiscoverySources::EMULATOR)
            .set_source(DiscoverySources::MDNS)
            .with_source(DiscoverySources::USB)
            .build();
        assert_eq!(discovery.notify_added, false);
        assert_eq!(discovery.notify_removed, false);
        assert_eq!(discovery.sources, DiscoverySources::USB | DiscoverySources::MDNS);
        assert!(discovery.emulator_instance_root.is_none());
        Ok(())
    }

    #[test]
    fn test_discovery_builder_with_root() -> Result<()> {
        let discovery = Discovery::builder()
            .set_source(DiscoverySources::MANUAL)
            .with_emulator_instance_root(PathBuf::from_str("/tmp").expect("tmp is a valid path"))
            .build();

        assert_eq!(discovery.notify_added, true);
        assert_eq!(discovery.notify_removed, true);
        assert_eq!(discovery.sources, DiscoverySources::MANUAL | DiscoverySources::EMULATOR);
        assert_eq!(
            discovery.emulator_instance_root,
            Some(PathBuf::from_str("/tmp").expect("tmp is a valid path"))
        );
        Ok(())
    }

    ///////////////////////////////////////////////////////////////////////////
    ///  TargetStream tests
    ///////////////////////////////////////////////////////////////////////////

    fn true_target_filter(_handle: &TargetHandle) -> bool {
        true
    }

    fn zedboot_target_filter(handle: &TargetHandle) -> bool {
        match handle.state {
            TargetState::Zedboot => true,
            _ => false,
        }
    }

    #[fuchsia::test]
    async fn test_target_stream() -> Result<()> {
        let (sender, queue) = unbounded();

        let mut stream = TargetStream {
            filter: Some(Box::new(true_target_filter)),
            mdns_watcher: None,
            fastboot_usb_watcher: None,
            manual_targets_watcher: None,
            emulator_watcher: None,
            fastboot_file_watcher: None,
            queue,
            notify_added: true,
            notify_removed: true,
        };

        // Send a few events
        sender.unbounded_send(Ok(TargetEvent::Added(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        sender.unbounded_send(Ok(TargetEvent::Removed(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        assert_eq!(
            stream.next().await.unwrap().ok().unwrap(),
            TargetEvent::Added(TargetHandle {
                node_name: Some("Vin".to_string()),
                state: TargetState::Zedboot
            })
        );

        assert_eq!(
            stream.next().await.unwrap().ok().unwrap(),
            TargetEvent::Removed(TargetHandle {
                node_name: Some("Vin".to_string()),
                state: TargetState::Zedboot
            })
        );

        Ok(())
    }

    #[fuchsia::test]
    async fn test_target_stream_ignores_added() -> Result<()> {
        let (sender, queue) = unbounded();

        let mut stream = TargetStream {
            filter: Some(Box::new(true_target_filter)),
            mdns_watcher: None,
            fastboot_usb_watcher: None,
            manual_targets_watcher: None,
            emulator_watcher: None,
            fastboot_file_watcher: None,
            queue,
            notify_added: false,
            notify_removed: true,
        };

        // Send a few events
        sender.unbounded_send(Ok(TargetEvent::Added(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        sender.unbounded_send(Ok(TargetEvent::Removed(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        assert_eq!(
            stream.next().await.unwrap().ok().unwrap(),
            TargetEvent::Removed(TargetHandle {
                node_name: Some("Vin".to_string()),
                state: TargetState::Zedboot
            })
        );

        Ok(())
    }

    #[fuchsia::test]
    async fn test_target_stream_ignores_removed() -> Result<()> {
        let (sender, queue) = unbounded();

        let mut stream = TargetStream {
            filter: Some(Box::new(true_target_filter)),
            mdns_watcher: None,
            fastboot_usb_watcher: None,
            manual_targets_watcher: None,
            emulator_watcher: None,
            fastboot_file_watcher: None,
            queue,
            notify_added: true,
            notify_removed: false,
        };

        // Send a few events
        sender.unbounded_send(Ok(TargetEvent::Removed(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        sender.unbounded_send(Ok(TargetEvent::Added(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        assert_eq!(
            stream.next().await.unwrap().ok().unwrap(),
            TargetEvent::Added(TargetHandle {
                node_name: Some("Vin".to_string()),
                state: TargetState::Zedboot
            })
        );

        Ok(())
    }

    #[fuchsia::test]
    async fn test_target_stream_filtered() -> Result<()> {
        let (sender, queue) = unbounded();

        let mut stream = TargetStream {
            filter: Some(Box::new(zedboot_target_filter)),
            mdns_watcher: None,
            fastboot_usb_watcher: None,
            manual_targets_watcher: None,
            emulator_watcher: None,
            fastboot_file_watcher: None,
            queue,
            notify_added: true,
            notify_removed: true,
        };

        // Send a few events
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let addr = TargetAddr::from(socket);
        // This should not come into the queue since the target is not in zedboot
        sender.unbounded_send(Ok(TargetEvent::Added(TargetHandle {
            node_name: Some("Kelsier".to_string()),
            state: TargetState::Product(vec![addr]),
        })))?;

        sender.unbounded_send(Ok(TargetEvent::Added(TargetHandle {
            node_name: Some("Vin".to_string()),
            state: TargetState::Zedboot,
        })))?;

        assert_eq!(
            stream.next().await.unwrap().ok().unwrap(),
            TargetEvent::Added(TargetHandle {
                node_name: Some("Vin".to_string()),
                state: TargetState::Zedboot
            })
        );

        Ok(())
    }

    fn build_instance_file(dir: &PathBuf, name: &str) -> Result<File> {
        let new_instance_dir = dir.join(String::from(name));
        std::fs::create_dir_all(&new_instance_dir)?;
        let new_instance_engine_file = new_instance_dir.join("engine.json");
        use emulator_instance::EmulatorInstanceInfo;
        // Build the expected config JSON contents
        let mut instance_data = emulator_instance::EmulatorInstanceData::new_with_state(
            name,
            emulator_instance::EngineState::Running,
        );
        instance_data.set_pid(std::process::id());
        let config = instance_data.get_emulator_configuration_mut();
        config.host.networking = emulator_instance::NetworkingMode::User;
        config.host.port_map.insert(
            String::from("ssh"),
            emulator_instance::PortMapping { guest: 22, host: Some(3322) },
        );
        let config_str = serde_json::to_string(&instance_data)?;
        let mut config_file = File::create(&new_instance_engine_file)?;
        config_file.write_all(config_str.as_bytes())?;
        config_file.flush()?;
        Ok(config_file)
    }

    // This test has a race condition, which I (slgrady) have spent hours trying to find.
    // Apparently the notify crate in emulator_instance is for some reason not producing
    // the Create event for the new emulator file. The event is created in a separate
    // thread, so it's not an async issue. The watcher is not being dropped at that point.
    // The file is in fact created and placed on the filesystem; the watcher thread
    // is running at the point we are waiting for the event.  Giving up, since I believe
    // the race-condition only comes up in the artificial environment of a test case.
    // Normally, emulators are extended events, not just a fast creation of a single file.
    #[ignore]
    #[fuchsia::test]
    async fn test_target_stream_produces_emulator() -> Result<()> {
        use tempfile::tempdir;

        let _env = ffx_config::test_init().await.expect("Failed to initialize test env");

        // Create the emulator instance dir
        let temp = tempdir().expect("cannot get tempdir");
        let instance_dir = temp.path().to_path_buf();
        let emu_instances = emulator_instance::EmulatorInstances::new(instance_dir.clone());

        // Add a new emulator
        let config_file = build_instance_file(&instance_dir, "emu-data-instance")?;

        // Before waiting on devices, let's make sure we're actually getting the
        // emulator. (This shouldn't be necessary, but I've seen this test flake
        // by timing out, so this is a validity check.)
        let existing = emulator_instance::get_all_targets(&emu_instances)?;
        assert_eq!(existing.len(), 1);

        // Start watching the directory
        let mut stream = wait_for_devices(
            true_target_filter,
            Some(instance_dir.clone()),
            None,
            true,
            false,
            DiscoverySources::EMULATOR,
        )
        .await?;

        // Assert that the existing emulator is discovered
        let next =
            stream.next().await.expect("No event was waiting after watching for existing emulator");
        let next = next.expect("Getting emulator event failed");
        assert_eq!(
            next,
            // The node_name and the state both have to match the contents of the emu_config above.
            TargetEvent::Added(TargetHandle {
                // Name must correspond to "runtime:name" value in config
                node_name: Some("emu-data-instance".to_string()),
                // Addr must correspond to "host:port_map:sh:host" value in config
                state: TargetState::Product(vec![TargetAddr::from_str("127.0.0.1:3322")?]),
            })
        );

        // Add a new (different) emulator
        let config_file2 = build_instance_file(&instance_dir, "emu-data-instance2")?;
        std::thread::sleep(std::time::Duration::from_secs(1));
        // let existing = emulator_instance::get_all_targets().await?;
        // assert_eq!(existing.len(), 2);

        // Assert that the newly-created emulator is discovered
        let next =
            stream.next().await.expect("No event was waiting after watching for new emulator");
        let next = next.expect("Getting emulator event failed");
        assert_eq!(
            next,
            // The node_name and the state both have to match the contents of the emu_config above.
            TargetEvent::Added(TargetHandle {
                // Name must correspond to "runtime:name" value in config
                node_name: Some("emu-data-instance2".to_string()),
                // Addr must correspond to "host:port_map:sh:host" value in config
                state: TargetState::Product(vec![TargetAddr::from_str("127.0.0.1:3322")?]),
            })
        );

        drop(config_file);
        drop(config_file2);
        std::fs::remove_dir_all(&instance_dir)?;
        // TODO(325325761) -- re-enable when emulator Remove events are generated
        // correctly.
        // let next = stream
        //     .next()
        //     .await
        //     .unwrap();
        // let next = next.expect("Getting emulator event failed");
        // assert_eq!(
        //     next,
        //     // The node_name and the state both have to match the contents of the emu_config above.
        //     TargetEvent::Removed(TargetHandle {
        //         // Name must correspond to "runtime:name" value in config
        //         node_name: Some("fuchsia-emulator".to_string()),
        //         // Addr must correspond to "host:port_map:sh:host" value in config
        //         state: TargetState::Product(TargetAddr::from_str("127.0.0.1:33881")?),
        //     })
        // );

        Ok(())
    }
}
