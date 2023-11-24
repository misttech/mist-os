// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{lock_ordering::DiagnosticsCoreDumpList, logging::log_debug, task::Task};
use fuchsia_inspect::Node;
use fuchsia_inspect_contrib::nodes::BoundedListNode;
use fuchsia_zircon::AsHandleRef;
use lock_sequence::{LockBefore, Locked};
use starnix_lock::OrderedMutex;

/// The maximum number of failed tasks to record.
///
/// This number is arbitrary and we may want to make it configurable in the future.
const MAX_NUM_COREDUMPS: usize = 64;

/// The maximum length of an argv string to record.
///
/// This number is arbitrary and we may wand to make it configurable in the future.
const MAX_ARGV_LENGTH: usize = 128;

/// A list of recently coredumped tasks in Inspect.
pub struct CoreDumpList {
    list: OrderedMutex<BoundedListNode, DiagnosticsCoreDumpList>,
}

impl CoreDumpList {
    pub fn new(node: Node) -> Self {
        Self { list: OrderedMutex::new(BoundedListNode::new(node, MAX_NUM_COREDUMPS)) }
    }

    pub fn record_core_dump<L>(&self, locked: &mut Locked<'_, L>, task: &Task)
    where
        L: LockBefore<DiagnosticsCoreDumpList>,
    {
        let mut list = self.list.lock(locked);
        list.add_entry(|crash_node| {
            let process_koid = task
                .thread_group
                .process
                .get_koid()
                .expect("handles for processes with crashing threads are still valid");
            let thread_koid = task
                .thread
                .read()
                .as_ref()
                .expect("coredumps occur in tasks with associated threads")
                .get_koid()
                .expect("handles for crashing threads are still valid");
            let pid = task.thread_group.leader as i64;
            let mut argv = task
                .read_argv()
                .unwrap_or_else(|_| vec![b"<unknown>".to_vec()])
                .into_iter()
                .map(|a| String::from_utf8_lossy(&a).to_string())
                .collect::<Vec<_>>()
                .join(" ");

            let original_len = argv.len();
            argv.truncate(MAX_ARGV_LENGTH - 3);
            if argv.len() < original_len {
                argv.push_str("...");
            }

            log_debug!(pid, %argv, "Recording task with a coredump.");
            crash_node.record_uint("thread_koid", thread_koid.raw_koid());
            crash_node.record_uint("process_koid", process_koid.raw_koid());
            crash_node.record_int("pid", pid);
            crash_node.record_string("argv", argv);
        });
    }
}
