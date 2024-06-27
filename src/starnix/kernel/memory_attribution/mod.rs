// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod sync;

use std::collections::{HashMap, HashSet};
use std::iter;
use std::sync::{mpsc, Arc, Weak};

use attribution::{AttributionServer, AttributionServerHandle};
use fidl::AsHandleRef;
use starnix_sync::Mutex;
use starnix_uapi::pid_t;
use {fidl_fuchsia_memory_attribution as fattribution, fuchsia_zircon as zx};

use crate::task::{Kernel, Task, ThreadGroup};

/// If the PID table updates multiple times within this interval, we only rescan
/// it once, to reduce overhead.
const MINIMUM_RESCAN_INTERVAL: zx::Duration = zx::Duration::from_millis(100);

/// If a new code path is added which mutates the PID table without notifying
/// the scanner thread, this timeout ensures we will at least eventually unpark
/// and process the changes, albeit with a larger latency.
const MAXIMUM_RESCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

pub struct MemoryAttributionManager {
    /// Holds state for the hanging-get attribution protocol.
    memory_attribution_server: AttributionServerHandle,
}

struct InitialState {
    /// The initial set of processes running in this kernel.
    processes: HashSet<pid_t>,

    /// A mapping from PID to process identifiers.
    process_names: HashMap<pid_t, String>,
}

impl MemoryAttributionManager {
    pub fn new(kernel: Weak<Kernel>) -> Self {
        let (publisher_tx, publisher_rx) = mpsc::sync_channel(1);
        let weak_kernel = kernel.clone();
        let publisher_rx = Arc::new(Mutex::new(Some(publisher_rx)));
        let memory_attribution_server = AttributionServer::new(Box::new(move || {
            // Initial scan of the PID table when a client connects.
            let mut events = vec![];
            let Some(kernel) = weak_kernel.upgrade() else { return vec![] };
            let pids = kernel.pids.read();
            let mut processes: HashSet<pid_t> = HashSet::new();
            let mut process_names: HashMap<pid_t, String> = HashMap::new();
            for thread_group in pids.get_thread_groups() {
                if let Some(leader) = pids.get_task(thread_group.leader).upgrade() {
                    let name = get_thread_group_identifier(&thread_group);
                    events.append(&mut attribution_info_for_thread_group(name.clone(), &leader));
                    processes.insert(thread_group.leader);
                    process_names.insert(thread_group.leader, name);
                }
            }
            drop(pids);

            // Spawn the pid table monitoring thread once.
            if let Some(publisher_rx) = publisher_rx.lock().take() {
                let (initial_state_tx, initial_state_rx) = mpsc::sync_channel(1);
                initial_state_tx.send(InitialState { processes, process_names }).unwrap();
                let weak_kernel = weak_kernel.clone();
                let notifier = sync::spawn_thread(&kernel, move |waiter| {
                    Self::run(weak_kernel, publisher_rx, initial_state_rx, waiter);
                });
                kernel.pids.write().set_thread_group_notifier(notifier);
            }
            events
        }));

        let publisher = memory_attribution_server.new_publisher();
        _ = publisher_tx.send(publisher);

        Self { memory_attribution_server }
    }

    pub fn new_observer(
        &self,
        control_handle: fattribution::ProviderControlHandle,
    ) -> attribution::Observer {
        self.memory_attribution_server.new_observer(control_handle)
    }

    /// Monitor the kernel for incremental memory attribution updates and
    /// publish them via the memory update publisher.
    ///
    /// ## Arguments
    ///
    /// - kernel: Weak reference to the kernel state.
    /// - publisher: Receiver for a handle used to publish memory attribution updates.
    /// - initial_state: Receiver for the initial state of attribution.
    /// - waiter: Used to wait for thread group changes.
    ///
    fn run(
        kernel: Weak<Kernel>,
        publisher: mpsc::Receiver<attribution::Publisher>,
        initial_state: mpsc::Receiver<InitialState>,
        waiter: sync::Waiter,
    ) {
        let publisher = publisher.recv().unwrap();
        let initial_state = initial_state.recv().unwrap();
        let InitialState { mut processes, mut process_names } = initial_state;

        loop {
            waiter.wait(MAXIMUM_RESCAN_INTERVAL);

            let Some(kernel) = kernel.upgrade() else { break };
            let thread_groups = kernel.pids.read().get_thread_groups();
            let mut updates = vec![];

            // Find added processes.
            for thread_group in &thread_groups {
                let pid = thread_group.leader;
                if !processes.contains(&pid) {
                    if let Some(leader) = kernel.pids.read().get_task(thread_group.leader).upgrade()
                    {
                        let name = get_thread_group_identifier(&thread_group);
                        let mut update = attribution_info_for_thread_group(name.clone(), &leader);
                        processes.insert(pid);
                        process_names.insert(pid, name);
                        updates.append(&mut update);
                    }
                }
            }

            // Find removed processes.
            let new_processes: HashSet<pid_t> = thread_groups.iter().map(|tg| tg.leader).collect();
            for thread_group in processes.difference(&new_processes) {
                let name = process_names.remove(thread_group).unwrap();
                updates.push(fattribution::AttributionUpdate::Remove(
                    fattribution::Identifier::Part(name),
                ));
            }
            processes = new_processes;

            // If there are updates to send, send them now.
            if !updates.is_empty() {
                _ = publisher.on_update(updates);
            }

            zx::Time::after(MINIMUM_RESCAN_INTERVAL).sleep();
        }
    }
}

fn get_thread_group_identifier(thread_group: &ThreadGroup) -> String {
    // The system task has an invalid Zircon process handle.
    let name = thread_group.process.get_name().unwrap_or_else(|_| c"[system task]".to_owned());
    let name = name.to_string_lossy();
    let id = thread_group.leader;
    let name = format!("PID {id}: {name}");
    name
}

fn attribution_info_for_thread_group(
    name: String,
    leader: &Task,
) -> Vec<fattribution::AttributionUpdate> {
    let new = new_principal(name.clone());
    let updated = updated_principal(leader, name);
    iter::once(new).chain(updated.into_iter()).collect()
}

/// Builds a `NewPrincipal` event.
fn new_principal(name: String) -> fattribution::AttributionUpdate {
    let new = fattribution::AttributionUpdate::Add(fattribution::NewPrincipal {
        identifier: Some(fattribution::Identifier::Part(name)),
        type_: Some(fattribution::Type::Runnable),
        detailed_attribution: None,
        ..Default::default()
    });
    new
}

/// Builds an `UpdatedPrincipal` event. If the task has an invalid root VMAR, returns `None`.
fn updated_principal(leader: &Task, name: String) -> Option<fattribution::AttributionUpdate> {
    let mm = leader.mm();
    let Some(vmar_koid) = mm.root_vmar.get_koid().ok() else { return None };
    let update = fattribution::AttributionUpdate::Update(fattribution::UpdatedPrincipal {
        identifier: Some(fattribution::Identifier::Part(name)),
        resources: Some(fattribution::Resources::Data(vec![fattribution::Resource::KernelObject(
            vmar_koid.raw_koid(),
        )])),
        ..Default::default()
    });
    Some(update)
}
