// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuchsia::errors::map_to_status,
    anyhow::Error,
    async_trait::async_trait,
    bitflags::bitflags,
    fuchsia_async as fasync,
    fuchsia_zircon::{
        self as zx,
        sys::zx_page_request_command_t::{ZX_PAGER_VMO_DIRTY, ZX_PAGER_VMO_READ},
        AsHandleRef, PacketContents, PagerPacket, SignalPacket,
    },
    futures::{join, stream, Future, StreamExt},
    fxfs::{
        log::*,
        round::{round_down, round_up},
    },
    once_cell::sync::Lazy,
    std::{
        collections::{hash_map::Entry, HashMap},
        marker::{Send, Sync},
        mem::MaybeUninit,
        ops::Range,
        sync::{Arc, Mutex, Weak},
    },
    storage_device::buffer,
    vfs::execution_scope::ExecutionScope,
};

pub struct Pager {
    pager: zx::Pager,
    scope: ExecutionScope,
    executor: fasync::EHandle,
    inner: Arc<Inner>,
}

struct Inner {
    files: Mutex<HashMap<u64, FileHolder>>,
    port: zx::Port,
}

impl Inner {
    fn port_thread_lifecycle(self: Arc<Self>, scope: ExecutionScope) {
        debug!("Pager port thread started successfully");

        loop {
            match self.port.wait(zx::Time::INFINITE) {
                Ok(packet) => {
                    let Some(_guard) = scope.try_active_guard() else { break };
                    match packet.contents() {
                        PacketContents::Pager(contents) => {
                            self.receive_pager_packet(packet.key(), contents);
                        }
                        PacketContents::SignalOne(signals) => {
                            self.receive_signal_packet(packet.key(), signals);
                        }
                        PacketContents::User(_) => {
                            debug!("Pager port thread received signal to terminate");
                            break;
                        }
                        _ => unreachable!(), // We don't expect any other kinds of packets
                    }
                }
                Err(e) => error!(error = ?e, "Port::wait failed"),
            }
        }
    }

    fn receive_pager_packet(&self, key: u64, contents: PagerPacket) {
        let command = contents.command();
        if command != ZX_PAGER_VMO_READ && command != ZX_PAGER_VMO_DIRTY {
            return;
        }

        let file = {
            match self.files.lock().unwrap().get(&key) {
                Some(FileHolder::Strong(file)) => file.clone(),
                Some(FileHolder::Weak(file)) => {
                    if let Some(file) = file.upgrade() {
                        file
                    } else {
                        return;
                    }
                }
                _ => {
                    return;
                }
            }
        };

        match command {
            ZX_PAGER_VMO_READ => file.page_in(contents.range()),
            ZX_PAGER_VMO_DIRTY => file.mark_dirty(contents.range()),
            _ => unreachable!("Unhandled commands are filtered above"),
        }
    }

    fn receive_signal_packet(&self, key: u64, signals: SignalPacket) {
        assert!(signals.observed().contains(zx::Signals::VMO_ZERO_CHILDREN));

        // Check to see if there really are no children (which is necessary to avoid races) and, if
        // so, replaces the strong reference with a weak one and calls on_zero_children on the node.
        // If the file does have children, this asks the kernel to send us the ON_ZERO_CHILDREN
        // notification for the file.
        let mut files = self.files.lock().unwrap();
        if let Some(holder) = files.get_mut(&key) {
            if let FileHolder::Strong(file) = holder {
                match file.vmo().info() {
                    Ok(info) => {
                        if info.num_children == 0 {
                            // Downgrade to a weak reference. Keep a strong reference until we
                            // drop the lock because otherwise there's the potential to deadlock
                            // (when the file is dropped, it will call unregister_file which
                            // needs to take the lock).
                            let weak = Arc::downgrade(&file);
                            let FileHolder::Strong(file)
                                = std::mem::replace(holder, FileHolder::Weak(weak))
                            else {
                                unreachable!()
                            };
                            // Drop the lock.
                            std::mem::drop(files);
                            file.on_zero_children();
                        } else {
                            // There's not much we can do here if this fails, so we panic.
                            watch_for_zero_children(&self.port, file.as_ref()).unwrap();
                        }
                    }
                    Err(e) => error!(error = ?e, "Vmo::info failed"),
                }
            }
        }
    }
}

// FileHolder is used to retain either a strong or a weak reference to a file.  If there are any
// child VMOs that have been shared, then we will have a strong reference which is required to keep
// the file alive.  When we detect that there are no more children, we can downgrade to a weak
// reference which will allow the file to be cleaned up if there are no other uses.
enum FileHolder {
    Strong(Arc<dyn PagerBacked>),
    Weak(Weak<dyn PagerBacked>),
}

impl FileHolder {
    fn as_ptr(&self) -> *const () {
        match self {
            FileHolder::Strong(file) => Arc::as_ptr(file) as *const (),
            FileHolder::Weak(file) => Weak::as_ptr(file) as *const (),
        }
    }
}

fn watch_for_zero_children(port: &zx::Port, file: &dyn PagerBacked) -> Result<(), zx::Status> {
    file.vmo().as_handle_ref().wait_async_handle(
        port,
        file.pager_key(),
        zx::Signals::VMO_ZERO_CHILDREN,
        zx::WaitAsyncOpts::empty(),
    )
}

impl From<Arc<dyn PagerBacked>> for FileHolder {
    fn from(file: Arc<dyn PagerBacked>) -> FileHolder {
        FileHolder::Strong(file)
    }
}

impl From<Weak<dyn PagerBacked>> for FileHolder {
    fn from(file: Weak<dyn PagerBacked>) -> FileHolder {
        FileHolder::Weak(file)
    }
}

/// Pager handles page requests. It is a per-volume object.
impl Pager {
    /// Creates a new pager.
    pub fn new(scope: ExecutionScope) -> Result<Self, Error> {
        let pager = zx::Pager::create(zx::PagerOptions::empty())?;
        let inner =
            Arc::new(Inner { files: Mutex::new(HashMap::default()), port: zx::Port::create() });
        {
            let inner = inner.clone();
            let scope = scope.clone();
            std::thread::spawn(|| inner.port_thread_lifecycle(scope));
        }
        Ok(Pager { pager, scope, executor: fasync::EHandle::local(), inner })
    }

    /// Spawns a short term task for the pager that includes a guard that will prevent termination.
    pub fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let guard = self.scope.active_guard();
        self.executor.spawn_detached(async move {
            task.await;
            std::mem::drop(guard);
        });
    }

    /// Creates a new VMO to be used with the pager. Page requests will not be serviced until
    /// [`Pager::register_file()`] is called.
    pub fn create_vmo(&self, pager_key: u64, initial_size: u64) -> Result<zx::Vmo, Error> {
        Ok(self.pager.create_vmo(
            zx::VmoOptions::RESIZABLE | zx::VmoOptions::TRAP_DIRTY,
            &self.inner.port,
            pager_key,
            initial_size,
        )?)
    }

    /// Registers a file with the pager.
    pub fn register_file(&self, file: &Arc<impl PagerBacked>) -> u64 {
        let pager_key = file.pager_key();
        self.inner
            .files
            .lock()
            .unwrap()
            .insert(pager_key, FileHolder::Weak(Arc::downgrade(file) as Weak<dyn PagerBacked>));
        pager_key
    }

    /// Unregisters a file with the pager.
    pub fn unregister_file(&self, file: &dyn PagerBacked) {
        if let Entry::Occupied(o) = self.inner.files.lock().unwrap().entry(file.pager_key()) {
            if std::ptr::eq(file as *const _ as *const (), o.get().as_ptr()) {
                if let FileHolder::Strong(file) = o.remove() {
                    file.on_zero_children();
                }
            }
        }
    }

    /// Starts watching for the `VMO_ZERO_CHILDREN` signal on `file`'s vmo. Returns false if the
    /// signal is already being watched for. When the pager receives the `VMO_ZERO_CHILDREN` signal
    /// [`PagerBacked::on_zero_children`] will be called.
    pub fn watch_for_zero_children(&self, file: &dyn PagerBacked) -> Result<bool, Error> {
        let mut files = self.inner.files.lock().unwrap();
        let file = files.get_mut(&file.pager_key()).unwrap();

        if let FileHolder::Weak(weak) = file {
            // Should never fail because watch_for_zero_children should be called from `file`.
            let strong = weak.upgrade().unwrap();

            watch_for_zero_children(&self.inner.port, strong.as_ref())?;

            *file = FileHolder::Strong(strong);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Terminates the pager, queueing a message to stop the port thread.
    pub fn terminate(&self) {
        let files = std::mem::take(&mut *self.inner.files.lock().unwrap());
        for (_, file) in files {
            if let FileHolder::Strong(file) = file {
                file.on_zero_children();
            }
        }
        // Queue a packet on the port to notify the thread to terminate.
        self.inner
            .port
            .queue(&zx::Packet::from_user_packet(0, 0, zx::UserPacket::from_u8_array([0; 32])))
            .unwrap();
    }

    /// Supplies pages in response to a `ZX_PAGER_VMO_READ` page request. See
    /// `zx_pager_supply_pages` for more information.
    pub fn supply_pages(
        &self,
        vmo: &zx::Vmo,
        range: Range<u64>,
        transfer_vmo: &zx::Vmo,
        transfer_offset: u64,
    ) {
        if let Err(e) = self.pager.supply_pages(vmo, range, transfer_vmo, transfer_offset) {
            error!(error = ?e, "supply_pages failed");
        }
    }

    /// Notifies the kernel that a page request for the given `range` has failed. Sent in response
    /// to a `ZX_PAGER_VMO_READ` or `ZX_PAGER_VMO_DIRTY` page request. See `ZX_PAGER_OP_FAIL` for
    /// more information.
    pub fn report_failure(&self, vmo: &zx::Vmo, range: Range<u64>, status: zx::Status) {
        let pager_status = match status {
            zx::Status::IO_DATA_INTEGRITY => zx::Status::IO_DATA_INTEGRITY,
            zx::Status::NO_SPACE => zx::Status::NO_SPACE,
            zx::Status::FILE_BIG => zx::Status::BUFFER_TOO_SMALL,
            zx::Status::IO
            | zx::Status::IO_DATA_LOSS
            | zx::Status::IO_INVALID
            | zx::Status::IO_MISSED_DEADLINE
            | zx::Status::IO_NOT_PRESENT
            | zx::Status::IO_OVERRUN
            | zx::Status::IO_REFUSED
            | zx::Status::PEER_CLOSED => zx::Status::IO,
            _ => zx::Status::BAD_STATE,
        };
        if let Err(e) = self.pager.op_range(zx::PagerOp::Fail(pager_status), vmo, range) {
            error!(error = ?e, "op_range failed");
        }
    }

    /// Allows the kernel to dirty the `range` of pages. Sent in response to a `ZX_PAGER_VMO_DIRTY`
    /// page request. See `ZX_PAGER_OP_DIRTY` for more information.
    pub fn dirty_pages(&self, vmo: &zx::Vmo, range: Range<u64>) {
        if let Err(e) = self.pager.op_range(zx::PagerOp::Dirty, vmo, range) {
            error!(error = ?e, "dirty_pages failed");
        }
    }

    /// Notifies the kernel that the filesystem has started cleaning the `range` of pages. See
    /// `ZX_PAGER_OP_WRITEBACK_BEGIN` for more information.
    pub fn writeback_begin(
        &self,
        vmo: &zx::Vmo,
        range: Range<u64>,
        options: zx::PagerWritebackBeginOptions,
    ) {
        if let Err(e) = self.pager.op_range(zx::PagerOp::WritebackBegin(options), vmo, range) {
            error!(error = ?e, "writeback_begin failed");
        }
    }

    /// Notifies the kernel that the filesystem has finished cleaning the `range` of pages. See
    /// `ZX_PAGER_OP_WRITEBACK_END` for more information.
    pub fn writeback_end(&self, vmo: &zx::Vmo, range: Range<u64>) {
        if let Err(e) = self.pager.op_range(zx::PagerOp::WritebackEnd, vmo, range) {
            error!(error = ?e, "writeback_end failed");
        }
    }

    /// Queries the `vmo` for ranges that are dirty within `range`. Returns `(num_returned,
    /// num_remaining)` where `num_returned` is the number of objects populated in `buffer` and
    /// `num_remaining` is the number of dirty ranges remaining in `range` that could not fit in
    /// `buffer`. See `zx_pager_query_dirty_ranges` for more information.
    pub fn query_dirty_ranges(
        &self,
        vmo: &zx::Vmo,
        range: Range<u64>,
        buffer: &mut [VmoDirtyRange],
    ) -> Result<(usize, usize), zx::Status> {
        let mut actual = 0;
        let mut avail = 0;
        let status = unsafe {
            // TODO(fxbug.dev/63989) Move to src/lib/zircon/rust/src/pager.rs once
            // query_dirty_ranges is part of the stable vDSO.
            zx::sys::zx_pager_query_dirty_ranges(
                self.pager.raw_handle(),
                vmo.raw_handle(),
                range.start,
                range.end - range.start,
                buffer.as_mut_ptr() as *mut u8,
                std::mem::size_of_val(buffer),
                &mut actual as *mut usize,
                &mut avail as *mut usize,
            )
        };
        zx::ok(status).map(|_| (actual, avail - actual))
    }

    /// Queries the `vmo` for any pager related statistics. If
    /// `PagerVmoStatsOptions::RESET_VMO_STATS` is passed then the stats will also be reset. See
    /// `zx_pager_query_vmo_stats` for more information.
    pub fn query_vmo_stats(
        &self,
        vmo: &zx::Vmo,
        options: PagerVmoStatsOptions,
    ) -> Result<PagerVmoStats, zx::Status> {
        #[repr(C)]
        #[derive(Default)]
        struct zx_pager_vmo_stats {
            pub modified: u32,
        }
        const ZX_PAGER_VMO_STATS_MODIFIED: u32 = 1;
        let mut vmo_stats = MaybeUninit::<zx_pager_vmo_stats>::uninit();
        let status = unsafe {
            // TODO(fxbug.dev/63989) Move to src/lib/zircon/rust/src/pager.rs once
            // query_vmo_stats is part of the stable vDSO.
            zx::sys::zx_pager_query_vmo_stats(
                self.pager.raw_handle(),
                vmo.raw_handle(),
                options.bits(),
                vmo_stats.as_mut_ptr() as *mut u8,
                std::mem::size_of::<zx_pager_vmo_stats>(),
            )
        };
        zx::ok(status)?;
        let vmo_stats = unsafe { vmo_stats.assume_init() };
        Ok(PagerVmoStats { was_vmo_modified: vmo_stats.modified == ZX_PAGER_VMO_STATS_MODIFIED })
    }
}

/// This is a trait for objects (files/blobs) that expose a pager backed VMO.
#[async_trait]
pub trait PagerBacked: Sync + Send + 'static {
    /// The pager backing this VMO.
    fn pager(&self) -> &Pager;

    /// The pager key passed to [`Pager::create_vmo`].
    fn pager_key(&self) -> u64;

    /// The pager backed VMO that this object is handling packets for. The VMO must be created with
    /// [`Pager::create_vmo`].
    fn vmo(&self) -> &zx::Vmo;

    /// Called by the pager when a `ZX_PAGER_VMO_READ` packet is received for the VMO. The
    /// implementation should respond by calling `Pager::supply_pages` or `Pager::report_failure`.
    fn page_in(self: Arc<Self>, range: Range<u64>);

    /// Called by the pager when a `ZX_PAGER_VMO_DIRTY` packet is received for the VMO. The
    /// implementation should respond by calling `Pager::dirty_pages` or `Pager::report_failure`.
    fn mark_dirty(self: Arc<Self>, range: Range<u64>);

    /// Called by the pager to indicate there are no more VMO children.
    fn on_zero_children(self: Arc<Self>);

    /// Total bytes readable. Anything reads over this will be zero padded in the VMO.
    fn byte_size(&self) -> u64;

    /// The alignment (in bytes) at which block aligned reads must be performed.
    /// This may be larger than the system page size (e.g. for compressed chunks).
    fn read_alignment(&self) -> u64;

    /// Reads one or more blocks into a buffer and returns it.
    /// Note that |aligned_byte_range| *must* be aligned to a
    /// multiple of |self.read_alignment()|.
    async fn aligned_read(
        &self,
        _aligned_byte_range: std::ops::Range<u64>,
    ) -> Result<(buffer::Buffer<'_>, usize), Error>;
}

/// A generic page_in implementation that supplies pages using block-aligned reads.
pub fn default_page_in<P: PagerBacked>(this: Arc<P>, mut range: Range<u64>) {
    let read_alignment = this.read_alignment();
    let byte_size = round_up(this.byte_size(), read_alignment).unwrap();

    // Zero-pad the tail if requested range exceeds the size of the thing we're reading.
    const ZERO_VMO_SIZE: u64 = 1_048_576;
    static ZERO_VMO: Lazy<zx::Vmo> = Lazy::new(|| zx::Vmo::create(ZERO_VMO_SIZE).unwrap());
    let mut offset = std::cmp::max(range.start, byte_size);
    while offset < range.end {
        let end = std::cmp::min(range.end, offset + ZERO_VMO_SIZE);
        this.pager().supply_pages(this.vmo(), offset..end, &ZERO_VMO, 0);
        offset = end;
    }

    if range.start % read_alignment != 0 {
        tracing::warn!(
            "Misaligned range.start in DefaultPagerBackedVmo::default_page_in. {} % {}",
            range.start,
            read_alignment
        );
        range.start = round_down(range.start, read_alignment);
    }
    if range.end % read_alignment != 0 {
        tracing::warn!(
            "Misaligned range.end in DefaultPagerBackedVmo::default_page_in. {} % {}",
            range.end,
            read_alignment
        );
        range.end = round_up(range.end, read_alignment).unwrap();
    }
    let mut read_size = round_down(TRANSFER_BUFFER_MAX_SIZE, read_alignment);

    if read_size == 0 {
        // TODO(fxbug.dev/299040602): If/when we can be sure compressed chunks are not larger
        // than transfer buffers, we can simplify this. For now, we must read a whole block
        // and use multiple transfer buffers in this case.
        read_size = read_alignment;
        // Maximum supported read size is 8MiB at time of writing. This is sufficient
        // for 1023*8MiB = ~8GiB blobs which is much larger than we need.
        assert!(read_size < TRANSFER_BUFFER_COUNT * TRANSFER_BUFFER_MAX_SIZE);
    }

    let mut futures = Vec::with_capacity(8);
    while range.start < range.end {
        let read_range = range.start..std::cmp::min(range.end, range.start + read_size);
        range.start = read_range.end;

        static TRANSFER_BUFFERS: Lazy<TransferBuffers> = Lazy::new(|| TransferBuffers::new());
        let this = this.clone();
        futures.push(async move {
            let num_tx_buffers =
                (read_size + TRANSFER_BUFFER_MAX_SIZE - 1) / TRANSFER_BUFFER_MAX_SIZE;
            let (buffer_result, tx_buffers) = join!(this.aligned_read(read_range.clone()), async {
                // Committing pages in the kernel is time consuming, so we do this in
                // parallel to the read.  This assumes that the implementation of join!
                // polls the other future first (which happens to be the case for now).
                let mut tx_buffers = Vec::new();
                for i in 0..num_tx_buffers {
                    let buffer = TRANSFER_BUFFERS.get().await;
                    let commit_size = std::cmp::min(
                        TRANSFER_BUFFER_MAX_SIZE,
                        read_range.end - (read_range.start + i * TRANSFER_BUFFER_MAX_SIZE),
                    );
                    buffer.commit(commit_size);
                    tx_buffers.push(buffer);
                }
                tx_buffers
            });
            let (buffer, buffer_len) = match buffer_result {
                Ok(v) => v,
                Err(e) => {
                    error!(
                        range = ?read_range,
                        pager_key = ?this.pager_key(),
                        ?byte_size,
                        error = ?e,
                        "Failed to load range"
                    );
                    this.pager().report_failure(this.vmo(), read_range.clone(), map_to_status(e));
                    return;
                }
            };
            let buf = &buffer.as_slice()[..buffer_len];
            let mut buf_range = read_range.start
                ..round_up(read_range.start + buffer_len as u64, zx::system_get_page_size())
                    .unwrap();
            for (buf, transfer_buffer) in
                std::iter::zip(buf.chunks(TRANSFER_BUFFER_MAX_SIZE as usize), tx_buffers)
            {
                let supply_range = buf_range.start
                    ..round_up(buf_range.start + buf.len() as u64, zx::system_get_page_size())
                        .unwrap();
                buf_range.start += buf.len() as u64;
                match transfer_buffer.vmo().write(buf, transfer_buffer.offset()) {
                    Ok(_) => {
                        this.pager().supply_pages(
                            this.vmo(),
                            supply_range.clone(),
                            transfer_buffer.vmo(),
                            transfer_buffer.offset(),
                        );
                    }
                    Err(e) => {
                        // Failures here due to OOM will get reported as IO errors, as those are
                        // considered transient.
                        error!(
                        range = ?supply_range,
                        pager_key = ?this.pager_key(),
                        error = ?e,
                        "Failed to transfer range");
                        this.pager().report_failure(
                            this.vmo(),
                            supply_range.clone(),
                            zx::Status::IO,
                        );
                    }
                }
            }
        });
    }

    // TODO(fxbug.dev/299206422): Cap concurrency and prevent parallelism until we have a
    // solution for buffer allocation failing.
    this.pager().spawn(stream::iter(futures).buffer_unordered(8).collect());
}

/// Represents a dirty range of page aligned bytes within a pager backed VMO.
#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct VmoDirtyRange {
    offset: u64,
    length: u64,
    options: u64,
}

impl VmoDirtyRange {
    /// The page aligned byte range.
    pub fn range(&self) -> Range<u64> {
        self.offset..(self.offset + self.length)
    }

    /// Returns true if all of the bytes in the range are 0.
    pub fn is_zero_range(&self) -> bool {
        self.options & zx::sys::ZX_VMO_DIRTY_RANGE_IS_ZERO != 0
    }
}

bitflags! {
    /// Options for `Pager::query_vmo_stats`.
    #[repr(transparent)]
    pub struct PagerVmoStatsOptions: u32 {
        /// Resets the stats at the of the `Pager::query_vmo_stats` call.
        const RESET_VMO_STATS = 1;
    }
}

/// Pager related statistic for a VMO.
pub struct PagerVmoStats {
    was_vmo_modified: bool,
}

impl PagerVmoStats {
    /// Returns true if the VMO was modified since the last time the VMO stats were reset.
    pub fn was_vmo_modified(&self) -> bool {
        self.was_vmo_modified
    }
}

// Transfer buffers are to be used with supply_pages. supply_pages only works with pages that are
// unmapped, but we need the pages to be mapped so that we can decrypt and potentially verify
// checksums.  To keep things simple, the buffers are fixed size at 1 MiB which should cover most
// requests.
pub const TRANSFER_BUFFER_MAX_SIZE: u64 = 1_048_576;

// The number of transfer buffers we support.
const TRANSFER_BUFFER_COUNT: u64 = 8;

pub struct TransferBuffers {
    vmo: zx::Vmo,
    free_list: Mutex<Vec<u64>>,
    event: event_listener::Event,
}

impl TransferBuffers {
    pub fn new() -> Self {
        const VMO_SIZE: u64 = TRANSFER_BUFFER_COUNT * TRANSFER_BUFFER_MAX_SIZE;
        Self {
            vmo: zx::Vmo::create(VMO_SIZE).unwrap(),
            free_list: Mutex::new(
                (0..VMO_SIZE).step_by(TRANSFER_BUFFER_MAX_SIZE as usize).collect(),
            ),
            event: event_listener::Event::new(),
        }
    }

    pub async fn get(&self) -> TransferBuffer<'_> {
        loop {
            let listener = self.event.listen();
            if let Some(offset) = self.free_list.lock().unwrap().pop() {
                return TransferBuffer { buffers: self, offset };
            }
            listener.await;
        }
    }
}

pub struct TransferBuffer<'a> {
    buffers: &'a TransferBuffers,

    // The offset this buffer starts at in the VMO.
    offset: u64,
}

impl TransferBuffer<'_> {
    pub fn vmo(&self) -> &zx::Vmo {
        &self.buffers.vmo
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    // Allocating pages in the kernel is time-consuming, so it can help to commit pages first,
    // whilst other work is occurring in the background, and then copy later which is relatively
    // fast.
    pub fn commit(&self, size: u64) {
        let _ignore_error = self.buffers.vmo.op_range(
            zx::VmoOp::COMMIT,
            self.offset,
            std::cmp::min(size, TRANSFER_BUFFER_MAX_SIZE),
        );
    }
}

impl Drop for TransferBuffer<'_> {
    fn drop(&mut self) {
        self.buffers.free_list.lock().unwrap().push(self.offset);
        self.buffers.event.notify(1);
    }
}

#[cfg(test)]
mod tests {
    use {super::*, futures::channel::mpsc, vfs::execution_scope::ExecutionScope};

    struct MockFile {
        vmo: zx::Vmo,
        pager_key: u64,
        pager: Arc<Pager>,
    }

    impl MockFile {
        fn new(pager: Arc<Pager>, pager_key: u64) -> Self {
            let vmo = pager.create_vmo(pager_key, zx::system_get_page_size().into()).unwrap();
            Self { pager, vmo, pager_key }
        }
    }

    #[async_trait]
    impl PagerBacked for MockFile {
        fn pager(&self) -> &Pager {
            &self.pager
        }

        fn pager_key(&self) -> u64 {
            self.pager_key
        }

        fn vmo(&self) -> &zx::Vmo {
            &self.vmo
        }

        fn page_in(self: Arc<Self>, range: Range<u64>) {
            let aux_vmo = zx::Vmo::create(range.end - range.start).unwrap();
            self.pager.supply_pages(&self.vmo, range, &aux_vmo, 0);
        }

        fn mark_dirty(self: Arc<Self>, range: Range<u64>) {
            self.pager.dirty_pages(&self.vmo, range);
        }

        fn on_zero_children(self: Arc<Self>) {}

        fn byte_size(&self) -> u64 {
            unimplemented!();
        }
        fn read_alignment(&self) -> u64 {
            unimplemented!();
        }
        async fn aligned_read(
            &self,
            _aligned_byte_range: std::ops::Range<u64>,
        ) -> Result<(buffer::Buffer<'_>, usize), Error> {
            unimplemented!();
        }
    }

    #[fuchsia::test(threads = 10)]
    async fn test_do_not_unregister_a_file_that_has_been_replaced() {
        const PAGER_KEY: u64 = 1234;
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope.clone()).unwrap());

        let file1 = Arc::new(MockFile::new(pager.clone(), PAGER_KEY));
        assert_eq!(pager.register_file(&file1), PAGER_KEY);

        let file2 = Arc::new(MockFile::new(pager.clone(), PAGER_KEY));
        // Replaces `file1` with `file2`.
        assert_eq!(pager.register_file(&file2), PAGER_KEY);

        // Should be a no-op since `file1` was replaced.
        pager.unregister_file(file1.as_ref());

        // If `file2` did not replace `file1` or `file1` removed the registration of `file2` then
        // the pager packets will be dropped and the write call will hang.
        file2.vmo().write(&[0, 1, 2, 3, 4], 0).unwrap();

        pager.unregister_file(file2.as_ref());
        pager.terminate();
        scope.wait().await;
    }

    struct OnZeroChildrenFile {
        pager: Arc<Pager>,
        vmo: zx::Vmo,
        pager_key: u64,
        sender: Mutex<mpsc::UnboundedSender<()>>,
    }

    impl OnZeroChildrenFile {
        fn new(pager: Arc<Pager>, pager_key: u64, sender: mpsc::UnboundedSender<()>) -> Self {
            let vmo = pager.create_vmo(pager_key, zx::system_get_page_size().into()).unwrap();
            Self { pager, vmo, pager_key, sender: Mutex::new(sender) }
        }
    }

    #[async_trait]
    impl PagerBacked for OnZeroChildrenFile {
        fn pager(&self) -> &Pager {
            &self.pager
        }

        fn pager_key(&self) -> u64 {
            self.pager_key
        }

        fn vmo(&self) -> &zx::Vmo {
            &self.vmo
        }

        fn page_in(self: Arc<Self>, _range: Range<u64>) {
            unreachable!();
        }

        fn mark_dirty(self: Arc<Self>, _range: Range<u64>) {
            unreachable!();
        }

        fn on_zero_children(self: Arc<Self>) {
            self.sender.lock().unwrap().unbounded_send(()).unwrap();
        }
        fn byte_size(&self) -> u64 {
            unreachable!();
        }
        fn read_alignment(&self) -> u64 {
            unreachable!();
        }
        async fn aligned_read(
            &self,
            _aligned_byte_range: std::ops::Range<u64>,
        ) -> Result<(buffer::Buffer<'_>, usize), Error> {
            unreachable!();
        }
    }

    #[fuchsia::test(threads = 10)]
    async fn test_watch_for_zero_children() {
        let (sender, mut receiver) = mpsc::unbounded();
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope.clone()).unwrap());
        let file = Arc::new(OnZeroChildrenFile::new(pager.clone(), 1234, sender));
        assert_eq!(pager.register_file(&file), file.pager_key());
        {
            let _child_vmo = file
                .vmo()
                .create_child(
                    zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE,
                    0,
                    file.vmo().get_size().unwrap(),
                )
                .unwrap();
            assert!(pager.watch_for_zero_children(file.as_ref()).unwrap());
        }
        // Wait for `on_zero_children` to be called.
        receiver.next().await.unwrap();

        pager.unregister_file(file.as_ref());
        pager.terminate();
        scope.wait().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_multiple_watch_for_zero_children_calls() {
        let (sender, mut receiver) = mpsc::unbounded();
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope.clone()).unwrap());
        let file = Arc::new(OnZeroChildrenFile::new(pager.clone(), 1234, sender));
        assert_eq!(pager.register_file(&file), file.pager_key());
        {
            let _child_vmo = file
                .vmo()
                .create_child(
                    zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE,
                    0,
                    file.vmo().get_size().unwrap(),
                )
                .unwrap();
            assert!(pager.watch_for_zero_children(file.as_ref()).unwrap());
            // `watch_for_zero_children` will return false when it's already watching.
            assert!(!pager.watch_for_zero_children(file.as_ref()).unwrap());
        }
        receiver.next().await.unwrap();

        // The pager stops listening for VMO_ZERO_CHILDREN once the signal fires. Calling
        // `watch_for_zero_children` afterwards should return true again because watching had
        // stopped.
        assert!(pager.watch_for_zero_children(file.as_ref()).unwrap());

        pager.unregister_file(file.as_ref());
        pager.terminate();
        scope.wait().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_status_code_mapping() {
        struct StatusCodeFile {
            vmo: zx::Vmo,
            pager_key: u64,
            pager: Arc<Pager>,
            status_code: Mutex<zx::Status>,
        }

        #[async_trait]
        impl PagerBacked for StatusCodeFile {
            fn pager(&self) -> &Pager {
                &self.pager
            }

            fn pager_key(&self) -> u64 {
                self.pager_key
            }

            fn vmo(&self) -> &zx::Vmo {
                &self.vmo
            }

            fn page_in(self: Arc<Self>, range: Range<u64>) {
                self.pager.report_failure(&self.vmo, range, *self.status_code.lock().unwrap())
            }

            fn mark_dirty(self: Arc<Self>, _range: Range<u64>) {
                unreachable!();
            }

            fn on_zero_children(self: Arc<Self>) {
                unreachable!();
            }

            fn byte_size(&self) -> u64 {
                unreachable!();
            }
            fn read_alignment(&self) -> u64 {
                unreachable!();
            }
            async fn aligned_read(
                &self,
                _aligned_byte_range: std::ops::Range<u64>,
            ) -> Result<(buffer::Buffer<'_>, usize), Error> {
                unreachable!();
            }
        }

        const PAGER_KEY: u64 = 1234;
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope.clone()).unwrap());
        let file = Arc::new(StatusCodeFile {
            vmo: pager.create_vmo(PAGER_KEY, zx::system_get_page_size().into()).unwrap(),
            pager_key: PAGER_KEY,
            pager: pager.clone(),
            status_code: Mutex::new(zx::Status::INTERNAL),
        });
        assert_eq!(pager.register_file(&file), PAGER_KEY);

        fn check_mapping(
            file: &StatusCodeFile,
            failure_code: zx::Status,
            expected_code: zx::Status,
        ) {
            {
                *file.status_code.lock().unwrap() = failure_code;
            }
            let mut buf = [0u8; 8];
            assert_eq!(file.vmo().read(&mut buf, 0).unwrap_err(), expected_code);
        }
        check_mapping(&file, zx::Status::IO_DATA_INTEGRITY, zx::Status::IO_DATA_INTEGRITY);
        check_mapping(&file, zx::Status::NO_SPACE, zx::Status::NO_SPACE);
        check_mapping(&file, zx::Status::FILE_BIG, zx::Status::BUFFER_TOO_SMALL);
        check_mapping(&file, zx::Status::IO, zx::Status::IO);
        check_mapping(&file, zx::Status::IO_DATA_LOSS, zx::Status::IO);
        check_mapping(&file, zx::Status::NOT_EMPTY, zx::Status::BAD_STATE);
        check_mapping(&file, zx::Status::BAD_STATE, zx::Status::BAD_STATE);

        pager.unregister_file(file.as_ref());
        pager.terminate();
        scope.wait().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_query_vmo_stats() {
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope.clone()).unwrap());
        let file = Arc::new(MockFile::new(pager.clone(), 1234));
        assert_eq!(pager.register_file(&file), file.pager_key());

        let stats = pager.query_vmo_stats(file.vmo(), PagerVmoStatsOptions::empty()).unwrap();
        // The VMO hasn't been modified yet.
        assert!(!stats.was_vmo_modified());

        file.vmo().write(&[0, 1, 2, 3, 4], 0).unwrap();
        let stats = pager.query_vmo_stats(file.vmo(), PagerVmoStatsOptions::empty()).unwrap();
        assert!(stats.was_vmo_modified());

        // Reset the stats this time.
        let stats =
            pager.query_vmo_stats(file.vmo(), PagerVmoStatsOptions::RESET_VMO_STATS).unwrap();
        // The stats weren't reset last time so the stats are still showing that the vmo is modified.
        assert!(stats.was_vmo_modified());

        let stats = pager.query_vmo_stats(file.vmo(), PagerVmoStatsOptions::empty()).unwrap();
        assert!(!stats.was_vmo_modified());

        pager.unregister_file(file.as_ref());
        pager.terminate();
        scope.wait().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_query_dirty_ranges() {
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope.clone()).unwrap());
        let file = Arc::new(MockFile::new(pager.clone(), 1234));
        assert_eq!(pager.register_file(&file), file.pager_key());

        let page_size: u64 = zx::system_get_page_size().into();
        file.vmo().set_size(page_size * 7).unwrap();
        // Modify the 2nd, 3rd, and 5th pages.
        file.vmo().write(&[1, 2, 3, 4], page_size).unwrap();
        file.vmo().write(&[1, 2, 3, 4], page_size * 2).unwrap();
        file.vmo().write(&[1, 2, 3, 4], page_size * 4).unwrap();

        let mut buffer = vec![VmoDirtyRange::default(); 3];
        let (actual, remaining) =
            pager.query_dirty_ranges(file.vmo(), 0..page_size * 7, &mut buffer).unwrap();
        assert_eq!(actual, 3);
        assert_eq!(remaining, 1);
        assert_eq!(buffer[0].range(), page_size..(page_size * 3));
        assert!(!buffer[0].is_zero_range());

        assert_eq!(buffer[1].range(), (page_size * 3)..(page_size * 4));
        assert!(buffer[1].is_zero_range());

        assert_eq!(buffer[2].range(), (page_size * 4)..(page_size * 5));
        assert!(!buffer[2].is_zero_range());

        let (actual, remaining) = pager
            .query_dirty_ranges(file.vmo(), page_size * 5..page_size * 7, &mut buffer)
            .unwrap();
        assert_eq!(actual, 1);
        assert_eq!(remaining, 0);
        assert_eq!(buffer[0].range(), (page_size * 5)..(page_size * 7));
        assert!(buffer[0].is_zero_range());

        pager.unregister_file(file.as_ref());
        pager.terminate();
        scope.wait().await;
    }
}
