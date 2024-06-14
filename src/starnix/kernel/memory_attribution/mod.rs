// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Arc, Weak};

use attribution::{AttributionServer, AttributionServerHandle};
use fidl::AsHandleRef;
use once_cell::sync::OnceCell;
use starnix_sync::Mutex;
use starnix_uapi::pid_t;
use {fidl_fuchsia_memory_attribution as fattribution, fuchsia_zircon as zx};

use crate::task::{Kernel, ThreadGroup};

const PID_TABLE_POLLING_INTERVAL: zx::Duration = zx::Duration::from_seconds(5);

pub struct MemoryAttributionManager {
    /// The kernel state.
    kernel: Weak<Kernel>,

    /// Holds state for the hanging-get attribution protocol.
    memory_attribution_server: OnceCell<AttributionServerHandle>,
}

impl MemoryAttributionManager {
    pub fn new(kernel: Weak<Kernel>) -> Self {
        Self { kernel, memory_attribution_server: Default::default() }
    }

    pub fn new_observer(
        &self,
        control_handle: fattribution::ProviderControlHandle,
    ) -> attribution::Observer {
        // Initialize the attribution server if not already.
        let server = self.memory_attribution_server.get_or_init(|| {
            let (publisher_tx, publisher_rx) = mpsc::sync_channel(1);
            let weak_kernel = self.kernel.clone();
            let publisher_rx = Arc::new(Mutex::new(Some(publisher_rx)));
            let server = AttributionServer::new(Box::new(move || {
                let mut events = vec![];
                let Some(kernel) = weak_kernel.upgrade() else { return vec![] };
                let pids = kernel.pids.read();
                let mut processes: HashSet<pid_t> = HashSet::new();
                let mut process_names: HashMap<pid_t, String> = HashMap::new();
                for thread_group in pids.get_thread_groups() {
                    let name = get_thread_group_identifier(&thread_group);
                    events.append(&mut attribution_info_for_thread_group(name.clone()));
                    processes.insert(thread_group.leader);
                    process_names.insert(thread_group.leader, name);
                }

                // Spawn the pid table monitoring thread once.
                if let Some(publisher_rx) = publisher_rx.lock().take() {
                    let weak_kernel = weak_kernel.clone();
                    kernel.kthreads.spawn(move |_, _| {
                        Self::run(weak_kernel, publisher_rx, processes, process_names)
                    });
                }
                return events;
            }));
            let publisher = server.new_publisher();
            _ = publisher_tx.send(publisher);
            server
        });

        server.new_observer(control_handle)
    }

    /// Monitor the kernel for incremental memory attribution updates and
    /// publish them via the memory update publisher.
    ///
    /// ## Arguments
    ///
    /// - kernel: Weak reference to the kernel state.
    /// - publisher: Receiver for a handle used to publish memory attribution updates.
    /// - processes: The initial set of processes running in this kernel.
    /// - process_names: A mapping from PID to process identifiers.
    ///
    fn run(
        kernel: Weak<Kernel>,
        publisher: mpsc::Receiver<attribution::Publisher>,
        mut processes: HashSet<pid_t>,
        mut process_names: HashMap<pid_t, String>,
    ) {
        let publisher = publisher.recv().unwrap();
        loop {
            let Some(kernel) = kernel.upgrade() else { break };
            let thread_groups = kernel.pids.read().get_thread_groups();
            let mut updates = vec![];

            // Find added processes.
            for thread_group in &thread_groups {
                let pid = thread_group.leader;
                if !processes.contains(&pid) {
                    let name = get_thread_group_identifier(&thread_group);
                    let mut update = attribution_info_for_thread_group(name.clone());
                    processes.insert(pid);
                    process_names.insert(pid, name);
                    updates.append(&mut update);
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

            zx::Time::after(PID_TABLE_POLLING_INTERVAL).sleep();
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

fn attribution_info_for_thread_group(name: String) -> Vec<fattribution::AttributionUpdate> {
    // NewPrincipal message
    let new = fattribution::AttributionUpdate::Add(fattribution::NewPrincipal {
        identifier: Some(fattribution::Identifier::Part(name)),
        type_: Some(fattribution::Type::Runnable),
        detailed_attribution: None,
        ..Default::default()
    });

    // TODO(https://fxbug.dev/337865227): Report the VMOs referenced by this process
    // in an UpdatedPrincipal message.

    vec![new]
}
