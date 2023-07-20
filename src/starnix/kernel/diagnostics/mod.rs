// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{lock::Mutex, logging::log_debug, task::Task};
use fuchsia_inspect::Node;
use fuchsia_inspect_contrib::nodes::BoundedListNode;

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
    list: Mutex<BoundedListNode>,
}

impl CoreDumpList {
    pub fn new(node: Node) -> Self {
        Self { list: Mutex::new(BoundedListNode::new(node, MAX_NUM_COREDUMPS)) }
    }

    pub fn record_core_dump(&self, task: &Task) {
        let mut list = self.list.lock();
        let crash_node = list.create_entry();

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
        crash_node.record_int("pid", pid);
        crash_node.record_string("argv", argv);
    }
}
