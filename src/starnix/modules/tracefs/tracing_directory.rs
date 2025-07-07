// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::event::{TraceEvent, TraceEventQueue};
use starnix_core::fileops_impl_nonseekable;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::buffers::InputBuffer;
use starnix_core::vfs::pseudo::simple_file::SimpleFileNode;
use starnix_core::vfs::{fileops_impl_noop_sync, FileObject, FileOps, FsNodeOps, OutputBuffer};
use starnix_logging::CATEGORY_TRACE_META;
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::errors::Errno;
use std::sync::Arc;

pub struct TraceMarkerFile {
    queue: Arc<TraceEventQueue>,
}

impl TraceMarkerFile {
    pub fn new_node(queue: Arc<TraceEventQueue>) -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(Self { queue: queue.clone() }))
    }
}

impl FileOps for TraceMarkerFile {
    fileops_impl_noop_sync!();
    fileops_impl_nonseekable!();

    fn read(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        Ok(0)
    }

    fn write(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        fuchsia_trace::duration!(CATEGORY_TRACE_META, c"Write atrace event");
        let mut bytes = data.read_all()?;
        // The TraceEvent struct appends a new line to the trace data unconditionally, so
        // remove the trailing newline if here to avoid generating empty events when reading.
        if bytes.ends_with(&['\n' as u8]) {
            bytes.truncate(bytes.len() - 1);
        }
        if self.queue.is_enabled() {
            let trace_event = TraceEvent::new(
                // This pid is a Kernel pid (do not confuse with userspace pid aka tgid), so we use
                // the task thread id, the pid and tid are equal when the thread is the "main thread"
                // of the thread group/process.
                // It is used when CPU scheduling information is not available.
                current_task.get_tid(),
                &bytes,
            );
            self.queue.push_event(trace_event)?;
        }
        return Ok(bytes.len());
    }
}
