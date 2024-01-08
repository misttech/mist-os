// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuchsia::{
        epochs::{Epochs, RefGuard},
        errors::map_to_status,
    },
    anyhow::Error,
    async_trait::async_trait,
    bitflags::bitflags,
    fuchsia_async as fasync,
    fuchsia_zircon::{
        self as zx,
        sys::zx_page_request_command_t::{ZX_PAGER_VMO_DIRTY, ZX_PAGER_VMO_READ},
        AsHandleRef, PacketContents, PagerPacket, SignalPacket,
    },
    futures::Future,
    fxfs::{
        log::*,
        round::{round_down, round_up},
    },
    once_cell::sync::Lazy,
    std::{
        marker::{Send, Sync},
        mem::MaybeUninit,
        ops::Range,
        sync::{Arc, Mutex, Weak},
    },
    storage_device::buffer,
    vfs::execution_scope::ExecutionScope,
};

fn watch_for_zero_children(file: &dyn PagerBacked) -> Result<(), zx::Status> {
    file.vmo().as_handle_ref().wait_async_handle(
        file.pager().executor.port(),
        file.pager_packet_receiver_registration().key(),
        zx::Signals::VMO_ZERO_CHILDREN,
        zx::WaitAsyncOpts::empty(),
    )
}

pub type PagerPacketReceiverRegistration = fasync::ReceiverRegistration<PagerPacketReceiver>;

/// A `fuchsia_async::PacketReceiver` that handles pager packets and the `VMO_ZERO_CHILDREN` signal.
pub struct PagerPacketReceiver {
    // The file should only ever be `None` for the brief period of time between `Pager::create_vmo`
    // and `Pager::register_file`. Nothing should be reading or writing to the vmo during that time
    // and `Pager::watch_for_zero_children` shouldn't be called either.
    file: Mutex<FileHolder>,
}

impl PagerPacketReceiver {
    /// Drops the strong reference to the file that might be held if
    /// `Pager::watch_for_zero_children` was called. This should only be used when forcibly dropping
    /// the file object. Calls `on_zero_children` if the strong reference was held.
    pub fn stop_watching_for_zero_children(&self) {
        let mut file = self.file.lock().unwrap();
        if let FileHolder::Strong(strong) = &*file {
            let weak = FileHolder::Weak(Arc::downgrade(&strong));
            let FileHolder::Strong(strong) = std::mem::replace(&mut *file, weak) else {
                unreachable!();
            };
            strong.on_zero_children();
        }
    }

    fn receive_pager_packet(&self, contents: PagerPacket) {
        let command = contents.command();
        if command != ZX_PAGER_VMO_READ && command != ZX_PAGER_VMO_DIRTY {
            return;
        }

        let file = match &*self.file.lock().unwrap() {
            FileHolder::Strong(file) => file.clone(),
            FileHolder::Weak(file) => {
                if let Some(file) = file.upgrade() {
                    file
                } else {
                    return;
                }
            }
            FileHolder::None => panic!("Pager::register_file was not called"),
        };

        let Some(_guard) = file.pager().scope.try_active_guard() else {
            // If an active guard can't be acquired then the filesystem must be shutting down. Fail
            // the page request to avoid leaving the client hanging.
            file.pager().report_failure(file.vmo(), contents.range(), zx::Status::BAD_STATE);
            return;
        };
        match command {
            ZX_PAGER_VMO_READ => file.page_in(contents.range()),
            ZX_PAGER_VMO_DIRTY => file.mark_dirty(contents.range()),
            _ => unreachable!("Unhandled commands are filtered above"),
        }
    }

    fn receive_signal_packet(&self, signals: SignalPacket) {
        assert!(signals.observed().contains(zx::Signals::VMO_ZERO_CHILDREN));

        // Check to see if there really are no children (which is necessary to avoid races) and, if
        // so, replace the strong reference with a weak one and call on_zero_children on the node.
        // If the file does have children, this asks the kernel to send us the ON_ZERO_CHILDREN
        // notification for the file.
        let mut file = self.file.lock().unwrap();
        if let FileHolder::Strong(strong) = &*file {
            match strong.vmo().info() {
                Ok(info) => {
                    if info.num_children == 0 {
                        let weak = FileHolder::Weak(Arc::downgrade(&strong));
                        let FileHolder::Strong(strong) = std::mem::replace(&mut *file, weak) else {
                            unreachable!();
                        };
                        strong.on_zero_children();
                    } else {
                        // There's not much we can do here if this fails, so we panic.
                        watch_for_zero_children(strong.as_ref()).unwrap();
                    }
                }
                Err(e) => error!(error = ?e, "Vmo::info failed"),
            }
        }
    }
}

impl fasync::PacketReceiver for PagerPacketReceiver {
    fn receive_packet(&self, packet: zx::Packet) {
        match packet.contents() {
            PacketContents::Pager(contents) => {
                self.receive_pager_packet(contents);
            }
            PacketContents::SignalOne(signals) => {
                self.receive_signal_packet(signals);
            }
            _ => unreachable!(), // We don't expect any other kinds of packets.
        }
    }
}

pub struct Pager {
    pager: zx::Pager,
    scope: ExecutionScope,
    executor: fasync::EHandle,

    // Whenever a file is flushed, we must make sure existing page requests for a file are completed
    // to eliminate the possibility of supplying stale data for a file.  We solve this by using a
    // barrier when we flush to wait for outstanding page requests to finish.  Technically, we only
    // need to wait for page requests for the specific file being flushed, but we should see if we
    // need to for performance reasons first.
    epochs: Arc<Epochs>,
}

// FileHolder is used to retain either a strong or a weak reference to a file.  If there are any
// child VMOs that have been shared, then we will have a strong reference which is required to keep
// the file alive.  When we detect that there are no more children, we can downgrade to a weak
// reference which will allow the file to be cleaned up if there are no other uses.
enum FileHolder {
    Strong(Arc<dyn PagerBacked>),
    Weak(Weak<dyn PagerBacked>),
    None,
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
        Ok(Pager {
            pager: zx::Pager::create(zx::PagerOptions::empty())?,
            scope,
            executor: fasync::EHandle::local(),
            epochs: Epochs::new(),
        })
    }

    /// Spawns a short term task for the pager that includes a guard that will prevent termination.
    pub fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let guard = self.scope.active_guard();
        self.executor.spawn_detached(async move {
            task.await;
            std::mem::drop(guard);
        });
    }

    /// Creates a new VMO to be used with the pager. `Pager::register_file` must be called before
    /// reading from, writing to, or creating children of the vmo.
    pub fn create_vmo(
        &self,
        initial_size: u64,
    ) -> Result<(zx::Vmo, PagerPacketReceiverRegistration), Error> {
        let registration = self.executor.register_receiver(Arc::new(PagerPacketReceiver {
            file: Mutex::new(FileHolder::None),
        }));
        Ok((
            self.pager.create_vmo(
                zx::VmoOptions::RESIZABLE | zx::VmoOptions::TRAP_DIRTY,
                self.executor.port(),
                registration.key(),
                initial_size,
            )?,
            registration,
        ))
    }

    /// Registers a file with the pager.
    pub fn register_file(&self, file: &Arc<impl PagerBacked>) {
        *file.pager_packet_receiver_registration().file.lock().unwrap() =
            FileHolder::Weak(Arc::downgrade(file) as Weak<dyn PagerBacked>);
    }

    /// Starts watching for the `VMO_ZERO_CHILDREN` signal on `file`'s vmo. Returns false if the
    /// signal is already being watched for. When the pager receives the `VMO_ZERO_CHILDREN` signal
    /// [`PagerBacked::on_zero_children`] will be called.
    pub fn watch_for_zero_children(&self, file: &dyn PagerBacked) -> Result<bool, Error> {
        let mut file = file.pager_packet_receiver_registration().file.lock().unwrap();

        match &*file {
            FileHolder::Weak(weak) => {
                // Should never fail because watch_for_zero_children should be called from `file`.
                let strong = weak.upgrade().unwrap();

                watch_for_zero_children(strong.as_ref())?;

                *file = FileHolder::Strong(strong);
                Ok(true)
            }
            FileHolder::Strong(_) => Ok(false),
            FileHolder::None => panic!("Pager::register_file was not called"),
        }
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
            // TODO(https://fxbug.dev/136457): The kernel can spuriously return ZX_ERR_NOT_FOUND.
            if e != zx::Status::NOT_FOUND {
                error!(error = ?e, "dirty_pages failed");
            }
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
            // TODO(https://fxbug.dev/63989) Move to src/lib/zircon/rust/src/pager.rs once
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
            // TODO(https://fxbug.dev/63989) Move to src/lib/zircon/rust/src/pager.rs once
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

    pub async fn page_in_barrier(&self) {
        self.epochs.barrier().await;
    }
}

/// This is a trait for objects (files/blobs) that expose a pager backed VMO.
#[async_trait]
pub trait PagerBacked: Sync + Send + 'static {
    /// The pager backing this VMO.
    fn pager(&self) -> &Pager;

    /// The receiver registration returned from [`Pager::create_vmo`].
    fn pager_packet_receiver_registration(&self) -> &PagerPacketReceiverRegistration;

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
    fxfs_trace::duration!(
        "start-page-in",
        "offset" => range.start,
        "len" => range.end - range.start
    );

    let pager = this.pager();

    let ref_guard = pager.epochs.add_ref();

    const ZERO_VMO_SIZE: u64 = 1_048_576;
    static ZERO_VMO: Lazy<zx::Vmo> = Lazy::new(|| zx::Vmo::create(ZERO_VMO_SIZE).unwrap());

    assert!(range.end < i64::MAX as u64);

    const READ_AHEAD_SIZE: u64 = 131_072;
    let read_alignment = this.read_alignment();
    let readahead_alignment = if read_alignment > READ_AHEAD_SIZE {
        read_alignment
    } else {
        round_down(READ_AHEAD_SIZE, read_alignment)
    };
    let aligned_size = round_up(this.byte_size(), read_alignment).unwrap();
    range = round_down(range.start, readahead_alignment)
        ..round_up(range.end, readahead_alignment).unwrap();
    if range.end > aligned_size {
        range.end = aligned_size;
    }

    // Zero-pad the tail if requested range exceeds the size of the thing we're reading.
    let mut offset = std::cmp::max(range.start, aligned_size);
    while offset < range.end {
        let end = std::cmp::min(range.end, offset + ZERO_VMO_SIZE);
        pager.supply_pages(this.vmo(), offset..end, &ZERO_VMO, 0);
        offset = end;
    }

    // Read in chunks of 128 KiB.
    let read_size = round_up(128 * 1024, read_alignment).unwrap();

    while range.start < range.end {
        let read_range = range.start..std::cmp::min(range.end, range.start + read_size);
        range.start += read_size;

        let this = this.clone();
        this.clone().pager().spawn(page_in_chunk(this, read_range, ref_guard.clone()));
    }
}

#[fxfs_trace::trace("offset" => read_range.start, "len" => read_range.end - read_range.start)]
async fn page_in_chunk<P: PagerBacked>(this: Arc<P>, read_range: Range<u64>, _ref_guard: RefGuard) {
    let (buffer, buffer_len) = match this.aligned_read(read_range.clone()).await {
        Ok(v) => v,
        Err(error) => {
            error!(range = ?read_range, ?error, "Failed to load range");
            this.pager().report_failure(this.vmo(), read_range.clone(), map_to_status(error));
            return;
        }
    };
    let supply_range = read_range.start
        ..round_up(read_range.start + buffer_len as u64, zx::system_get_page_size() as u64)
            .unwrap();
    this.pager().supply_pages(
        this.vmo(),
        supply_range,
        buffer.allocator().buffer_source().vmo(),
        buffer.range().start as u64,
    );
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
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

#[cfg(test)]
mod tests {
    use {
        super::*, futures::channel::mpsc, futures::StreamExt, vfs::execution_scope::ExecutionScope,
    };

    struct MockFile {
        vmo: zx::Vmo,
        pager_packet_receiver_registration: PagerPacketReceiverRegistration,
        pager: Arc<Pager>,
    }

    impl MockFile {
        fn new(pager: Arc<Pager>) -> Self {
            let (vmo, pager_packet_receiver_registration) =
                pager.create_vmo(zx::system_get_page_size().into()).unwrap();
            Self { pager, vmo, pager_packet_receiver_registration }
        }
    }

    #[async_trait]
    impl PagerBacked for MockFile {
        fn pager(&self) -> &Pager {
            &self.pager
        }

        fn pager_packet_receiver_registration(&self) -> &PagerPacketReceiverRegistration {
            &self.pager_packet_receiver_registration
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

    struct OnZeroChildrenFile {
        pager: Arc<Pager>,
        vmo: zx::Vmo,
        pager_packet_receiver_registration: PagerPacketReceiverRegistration,
        sender: Mutex<mpsc::UnboundedSender<()>>,
    }

    impl OnZeroChildrenFile {
        fn new(pager: Arc<Pager>, sender: mpsc::UnboundedSender<()>) -> Self {
            let (vmo, pager_packet_receiver_registration) =
                pager.create_vmo(zx::system_get_page_size().into()).unwrap();
            Self { pager, vmo, pager_packet_receiver_registration, sender: Mutex::new(sender) }
        }
    }

    #[async_trait]
    impl PagerBacked for OnZeroChildrenFile {
        fn pager(&self) -> &Pager {
            &self.pager
        }

        fn pager_packet_receiver_registration(&self) -> &PagerPacketReceiverRegistration {
            &self.pager_packet_receiver_registration
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
        let file = Arc::new(OnZeroChildrenFile::new(pager.clone(), sender));
        pager.register_file(&file);
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

        scope.wait().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_multiple_watch_for_zero_children_calls() {
        let (sender, mut receiver) = mpsc::unbounded();
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope.clone()).unwrap());
        let file = Arc::new(OnZeroChildrenFile::new(pager.clone(), sender));
        pager.register_file(&file);
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

        file.pager_packet_receiver_registration.stop_watching_for_zero_children();

        scope.wait().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_status_code_mapping() {
        struct StatusCodeFile {
            vmo: zx::Vmo,
            pager: Arc<Pager>,
            status_code: Mutex<zx::Status>,
            pager_packet_receiver_registration: PagerPacketReceiverRegistration,
        }

        #[async_trait]
        impl PagerBacked for StatusCodeFile {
            fn pager(&self) -> &Pager {
                &self.pager
            }

            fn pager_packet_receiver_registration(&self) -> &PagerPacketReceiverRegistration {
                &self.pager_packet_receiver_registration
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

        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope.clone()).unwrap());
        let (vmo, pager_packet_receiver_registration) =
            pager.create_vmo(zx::system_get_page_size().into()).unwrap();
        let file = Arc::new(StatusCodeFile {
            vmo,
            pager: pager.clone(),
            status_code: Mutex::new(zx::Status::INTERNAL),
            pager_packet_receiver_registration,
        });
        pager.register_file(&file);

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

        scope.wait().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_query_vmo_stats() {
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope.clone()).unwrap());
        let file = Arc::new(MockFile::new(pager.clone()));
        pager.register_file(&file);

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

        scope.wait().await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_query_dirty_ranges() {
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope.clone()).unwrap());
        let file = Arc::new(MockFile::new(pager.clone()));
        pager.register_file(&file);

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

        scope.wait().await;
    }
}
