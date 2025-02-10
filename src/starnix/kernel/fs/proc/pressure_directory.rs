// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{CurrentTask, EventHandler, WaitCanceler, Waiter};
use crate::vfs::buffers::InputBuffer;
use crate::vfs::{
    fileops_impl_delegate_read_and_seek, fileops_impl_noop_sync, DynamicFile, DynamicFileBuf,
    DynamicFileSource, FileObject, FileOps, FileSystemHandle, FsNodeHandle, FsNodeOps,
    SimpleFileNode, StaticDirectoryBuilder,
};
use fidl_fuchsia_starnix_psi::{
    PsiProviderGetMemoryPressureStatsResponse, PsiProviderSynchronousProxy,
};

use starnix_logging::{log_error, track_stub};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::vfs::FdEvents;
use std::sync::Arc;

/// Creates the /proc/pressure directory. https://docs.kernel.org/accounting/psi.html
pub fn pressure_directory(
    current_task: &CurrentTask,
    fs: &FileSystemHandle,
) -> Option<FsNodeHandle> {
    let Some(psi_provider) = current_task.kernel().psi_provider.get() else {
        return None;
    };

    let mut dir = StaticDirectoryBuilder::new(fs);
    dir.entry(
        current_task,
        "memory",
        MemoryPressureFile::new_node(psi_provider.clone()),
        mode!(IFREG, 0o666),
    );
    dir.entry(
        current_task,
        "cpu",
        StubPressureFile::new_node(StubPressureFileKind::CPU),
        mode!(IFREG, 0o666),
    );
    dir.entry(
        current_task,
        "io",
        StubPressureFile::new_node(StubPressureFileKind::IO),
        mode!(IFREG, 0o666),
    );
    Some(dir.build(current_task))
}

struct MemoryPressureFileSource {
    psi_provider: Arc<PsiProviderSynchronousProxy>,
}

impl DynamicFileSource for MemoryPressureFileSource {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let PsiProviderGetMemoryPressureStatsResponse { some, full, .. } = self
            .psi_provider
            .get_memory_pressure_stats(zx::MonotonicInstant::INFINITE)
            .map_err(|e| {
                log_error!("FIDL error getting memory pressure stats: {e}");
                errno!(EIO)
            })?
            .map_err(|_| errno!(EIO))?;

        let some = some.ok_or_else(|| errno!(EIO))?;
        writeln!(
            sink,
            "some avg10={:.2} avg60={:.2} avg300={:.2} total={}",
            some.avg10.ok_or_else(|| errno!(EIO))?,
            some.avg60.ok_or_else(|| errno!(EIO))?,
            some.avg300.ok_or_else(|| errno!(EIO))?,
            some.total.ok_or_else(|| errno!(EIO))? / 1000
        )?;

        let full = full.ok_or_else(|| errno!(EIO))?;
        writeln!(
            sink,
            "full avg10={:.2} avg60={:.2} avg300={:.2} total={}",
            full.avg10.ok_or_else(|| errno!(EIO))?,
            full.avg60.ok_or_else(|| errno!(EIO))?,
            full.avg300.ok_or_else(|| errno!(EIO))?,
            full.total.ok_or_else(|| errno!(EIO))? / 1000
        )?;

        Ok(())
    }
}

struct MemoryPressureFile {
    source: DynamicFile<MemoryPressureFileSource>,
}

impl MemoryPressureFile {
    pub fn new_node(psi_provider: Arc<PsiProviderSynchronousProxy>) -> impl FsNodeOps {
        SimpleFileNode::new(move || {
            Ok(Self {
                source: DynamicFile::new(MemoryPressureFileSource {
                    psi_provider: psi_provider.clone(),
                }),
            })
        })
    }
}

impl FileOps for MemoryPressureFile {
    fileops_impl_delegate_read_and_seek!(self, self.source);
    fileops_impl_noop_sync!();

    /// Pressure notifications are configured by writing to the file.
    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        // Ignore the request for now.

        track_stub!(TODO("https://fxbug.dev/322873423"), "memory pressure notification setup");
        Ok(data.drain())
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        _events: FdEvents,
        _handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(waiter.fake_wait())
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(FdEvents::empty())
    }
}

struct StubPressureFileSource;

impl DynamicFileSource for StubPressureFileSource {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        writeln!(sink, "some avg10=0.00 avg60=0.00 avg300=0.00 total=0")?;
        writeln!(sink, "full avg10=0.00 avg60=0.00 avg300=0.00 total=0")?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum StubPressureFileKind {
    CPU,
    IO,
}

struct StubPressureFile {
    kind: StubPressureFileKind,
    source: DynamicFile<StubPressureFileSource>,
}

impl StubPressureFile {
    pub fn new_node(kind: StubPressureFileKind) -> impl FsNodeOps {
        SimpleFileNode::new(move || {
            Ok(Self { kind, source: DynamicFile::new(StubPressureFileSource) })
        })
    }
}

impl FileOps for StubPressureFile {
    fileops_impl_delegate_read_and_seek!(self, self.source);
    fileops_impl_noop_sync!();

    /// Pressure notifications are configured by writing to the file.
    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        // Ignore the request for now.

        track_stub!(
            TODO("https://fxbug.dev/322873423"),
            match self.kind {
                StubPressureFileKind::CPU => "cpu pressure notification setup",
                StubPressureFileKind::IO => "io pressure notification setup",
            }
        );
        Ok(data.drain())
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        _events: FdEvents,
        _handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(waiter.fake_wait())
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(FdEvents::empty())
    }
}
