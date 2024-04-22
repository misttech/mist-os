// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuchsia::{
        epochs::{Epochs, RefGuard},
        errors::map_to_status,
        fxblob::blob::FxBlob,
        node::FxNode,
        profile::Recorder,
    },
    anyhow::Error,
    bitflags::bitflags,
    fuchsia_async as fasync,
    fuchsia_zircon::{
        self as zx,
        sys::zx_page_request_command_t::{ZX_PAGER_VMO_DIRTY, ZX_PAGER_VMO_READ},
        AsHandleRef, PacketContents, PagerPacket, SignalPacket,
    },
    fxfs::{
        log::*,
        range::RangeExt,
        round::{round_down, round_up},
    },
    once_cell::sync::Lazy,
    std::{
        future::Future,
        marker::PhantomData,
        mem::MaybeUninit,
        ops::Range,
        sync::{Arc, Mutex, Weak},
    },
    storage_device::buffer,
    vfs::execution_scope::ExecutionScope,
};

fn watch_for_zero_children(file: &impl PagerBacked) -> Result<(), zx::Status> {
    file.vmo().as_handle_ref().wait_async_handle(
        file.pager().executor.port(),
        file.pager_packet_receiver_registration().key(),
        zx::Signals::VMO_ZERO_CHILDREN,
        zx::WaitAsyncOpts::empty(),
    )
}

pub type PagerPacketReceiverRegistration<T> = fasync::ReceiverRegistration<PagerPacketReceiver<T>>;

/// A `fuchsia_async::PacketReceiver` that handles pager packets and the `VMO_ZERO_CHILDREN` signal.
pub struct PagerPacketReceiver<T> {
    // The file should only ever be `None` for the brief period of time between `Pager::create_vmo`
    // and `Pager::register_file`. Nothing should be reading or writing to the vmo during that time
    // and `Pager::watch_for_zero_children` shouldn't be called either.
    file: Mutex<FileHolder<T>>,
}

impl<T: PagerBacked> PagerPacketReceiver<T> {
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
                    error!("Received a page request for a file that is closed {:?}", contents);
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
            ZX_PAGER_VMO_READ => file.clone().page_in(PageInRange::new(contents.range(), file)),
            ZX_PAGER_VMO_DIRTY => {
                file.clone().mark_dirty(MarkDirtyRange::new(contents.range(), file))
            }
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

impl<T: PagerBacked> fasync::PacketReceiver for PagerPacketReceiver<T> {
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
    recorder: Mutex<Option<Recorder>>,
}

// FileHolder is used to retain either a strong or a weak reference to a file.  If there are any
// child VMOs that have been shared, then we will have a strong reference which is required to keep
// the file alive.  When we detect that there are no more children, we can downgrade to a weak
// reference which will allow the file to be cleaned up if there are no other uses.
enum FileHolder<T> {
    Strong(Arc<T>),
    Weak(Weak<T>),
    None,
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
            recorder: Mutex::new(None),
        })
    }

    /// Spawns a short term task for the pager that includes a guard that will prevent termination.
    fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let guard = self.scope.active_guard();
        self.executor.spawn_detached(async move {
            task.await;
            std::mem::drop(guard);
        });
    }

    pub fn set_recorder(&self, recorder: Option<Recorder>) {
        // Drop the old one outside of the lock.
        let _ = std::mem::replace(&mut (*self.recorder.lock().unwrap()), recorder);
    }

    pub fn record_page_in<P: PagerBacked>(&self, node: Arc<P>, range: Range<u64>) {
        if let Ok(blob) = node.into_any().downcast::<FxBlob>() {
            let mut recorder_holder = self.recorder.lock().unwrap();
            if let Some(recorder) = &mut (*recorder_holder) {
                // If the message fails to send, so will all the rest.
                if let Err(_) = recorder.record(&blob.root(), range.start) {
                    *recorder_holder = None;
                }
            }
        }
    }

    /// Creates a new VMO to be used with the pager. `Pager::register_file` must be called before
    /// reading from, writing to, or creating children of the vmo.
    pub fn create_vmo<T: PagerBacked>(
        &self,
        initial_size: u64,
        vmo_options: zx::VmoOptions,
    ) -> Result<(zx::Vmo, PagerPacketReceiverRegistration<T>), Error> {
        let registration = self.executor.register_receiver(Arc::new(PagerPacketReceiver {
            file: Mutex::new(FileHolder::None),
        }));
        Ok((
            self.pager.create_vmo(
                vmo_options,
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
            FileHolder::Weak(Arc::downgrade(file));
    }

    /// Starts watching for the `VMO_ZERO_CHILDREN` signal on `file`'s vmo. Returns false if the
    /// signal is already being watched for. When the pager receives the `VMO_ZERO_CHILDREN` signal
    /// [`PagerBacked::on_zero_children`] will be called.
    pub fn watch_for_zero_children(&self, file: &impl PagerBacked) -> Result<bool, Error> {
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
    fn supply_pages(
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
    fn report_failure(&self, vmo: &zx::Vmo, range: Range<u64>, status: zx::Status) {
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
    fn dirty_pages(&self, vmo: &zx::Vmo, range: Range<u64>) {
        if let Err(e) = self.pager.op_range(zx::PagerOp::Dirty, vmo, range) {
            // TODO(https://fxbug.dev/42086069): The kernel can spuriously return ZX_ERR_NOT_FOUND.
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
            // TODO(https://fxbug.dev/42142550) Move to src/lib/zircon/rust/src/pager.rs once
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
            // TODO(https://fxbug.dev/42142550) Move to src/lib/zircon/rust/src/pager.rs once
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
pub trait PagerBacked: FxNode + Sync + Send + Sized + 'static {
    /// The pager backing this VMO.
    fn pager(&self) -> &Pager;

    /// The receiver registration returned from [`Pager::create_vmo`].
    fn pager_packet_receiver_registration(&self) -> &PagerPacketReceiverRegistration<Self>;

    /// The pager backed VMO that this object is handling packets for. The VMO must be created with
    /// [`Pager::create_vmo`].
    fn vmo(&self) -> &zx::Vmo;

    /// Called by the pager when a `ZX_PAGER_VMO_READ` packet is received for the VMO. The
    /// implementation must respond by calling either `PageInRange::supply_pages` or
    /// `PageInRange::report_failure`.
    fn page_in(self: Arc<Self>, range: PageInRange<Self>);

    /// Called by the pager when a `ZX_PAGER_VMO_DIRTY` packet is received for the VMO. The
    /// implementation must respond by calling either `MarkDirtyRange::dirty_pages` or
    /// `MarkDirtyRange::report_failure`.
    fn mark_dirty(self: Arc<Self>, range: MarkDirtyRange<Self>);

    /// Called by the pager to indicate there are no more VMO children.
    fn on_zero_children(self: Arc<Self>);

    /// Total bytes readable. Anything reads over this will be zero padded in the VMO.
    fn byte_size(&self) -> u64;

    /// The alignment (in bytes) at which block aligned reads must be performed.
    /// This may be larger than the system page size (e.g. for compressed chunks).
    fn read_alignment(&self) -> u64;

    /// Reads one or more blocks into a buffer and returns it. `aligned_byte_range` will always
    /// start at a multiple of `self.read_alignment()` and will end at a multiple of
    /// `self.read_alignment()` unless that would extend beyond `self.byte_size()`, in which case,
    /// `aligned_byte_range` will end at `self.byte_size()`'s next page multiple. The returned
    /// buffer must be at least as large as the requested range. Only the requested range will be
    /// supplied to the pager.
    fn aligned_read(
        &self,
        aligned_byte_range: std::ops::Range<u64>,
    ) -> impl Future<Output = Result<buffer::Buffer<'_>, Error>> + Send;
}

/// A generic page_in implementation that supplies pages using block-aligned reads.
pub fn default_page_in<P: PagerBacked>(this: Arc<P>, pager_range: PageInRange<P>) {
    fxfs_trace::duration!(
        c"start-page-in",
        "offset" => pager_range.start(),
        "len" => pager_range.len()
    );

    let pager = this.pager();

    let ref_guard = pager.epochs.add_ref();

    const ZERO_VMO_SIZE: u64 = 1_048_576;
    static ZERO_VMO: Lazy<zx::Vmo> = Lazy::new(|| zx::Vmo::create(ZERO_VMO_SIZE).unwrap());

    assert!(pager_range.end() < i64::MAX as u64);

    const READ_AHEAD_SIZE: u64 = 128 * 1024;
    let read_alignment = this.read_alignment();
    let readahead_alignment = if read_alignment > READ_AHEAD_SIZE {
        read_alignment
    } else {
        round_down(READ_AHEAD_SIZE, read_alignment)
    };

    // Two important subtleties to consider in this space:
    //
    // `byte_size` is the official size of the object. VMOs are page-aligned so `page_aligned_size`
    // is the "official" page length of the object. This may be smaller than Vmo::get_size because
    // these two things are not updated atomically. The reverse is not true -- We do not currently
    // ever shrink a VMO's size. We also do not update byte_size (self.handle.get_size()) if an
    // independent handle is used to grow a file. This means the VMO's size should always be
    // strictly equal or bigger than `byte_size`.
    //
    // It is valid to supply more pages than asked, but supplying pages outside of the VMO range
    // will trigger OUT_OF_RANGE errors and the call will fail without supplying anything. We must
    // supply the range requested under all circumstances to unblock any page misses but we should
    // take care to never supply additional pages beyond `page_aligned_size` as there is a chance
    // that we might serve a range outside of the VMO and fail to supply anything at all.

    let page_aligned_size = round_up(this.byte_size(), page_size()).unwrap();

    // Zero-pad the tail if the requested range exceeds the size of the thing we're reading. This
    // can happen when we truncate and there are outstanding pager requests that the kernel was not
    // able to cancel in time.
    let (read_range, zero_range) = pager_range.split(page_aligned_size);
    if let Some(zero_range) = zero_range {
        for range in zero_range.chunks(ZERO_VMO_SIZE) {
            range.supply_pages(&ZERO_VMO, 0);
        }
    }

    if let Some(read_range) = read_range {
        let expanded_range_for_readahead = round_down(read_range.start(), readahead_alignment)
            ..std::cmp::min(
                round_up(read_range.end(), readahead_alignment).unwrap(),
                page_aligned_size,
            );
        let read_range = read_range.expand(expanded_range_for_readahead);
        for range in read_range.chunks(readahead_alignment) {
            let recorded_range = range.range.clone();
            this.pager().spawn(page_in_chunk(this.clone(), range, ref_guard.clone()));
            this.pager().record_page_in(this.clone(), recorded_range);
        }
    }
}

#[fxfs_trace::trace("offset" => read_range.start(), "len" => read_range.len())]
async fn page_in_chunk<P: PagerBacked>(
    this: Arc<P>,
    read_range: PageInRange<P>,
    _ref_guard: RefGuard,
) {
    let buffer = match this.aligned_read(read_range.range()).await {
        Ok(v) => v,
        Err(error) => {
            error!(range = ?read_range.range(), ?error, "Failed to load range");
            read_range.report_failure(map_to_status(error));
            return;
        }
    };
    assert!(
        buffer.len() as u64 >= read_range.len(),
        "A buffer smaller than requested was returned. requested: {}, returned: {}",
        read_range.len(),
        buffer.len()
    );
    read_range.supply_pages(buffer.allocator().buffer_source().vmo(), buffer.range().start as u64);
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

#[inline]
fn page_size() -> u64 {
    zx::system_get_page_size().into()
}

/// A trait for specializing `PagerRange` for different request types.
pub trait PagerRequestType {
    /// Returns the name of the request type for logging purposes.
    fn request_type_name() -> &'static str;
}

/// A request generated from a ZX_PAGER_VMO_READ packet.
pub struct PageInRequest;

impl PagerRequestType for PageInRequest {
    fn request_type_name() -> &'static str {
        "PageInRequest"
    }
}

/// The requested range from a ZX_PAGER_VMO_READ packet. This object must not be dropped without
/// calling either `supply_pages` or `report_failure`.
pub type PageInRange<T> = PagerRange<T, PageInRequest>;

/// A requested generated from a ZX_PAGER_VMO_DIRTY packet.
pub struct MarkDirtyRequest;

impl PagerRequestType for MarkDirtyRequest {
    fn request_type_name() -> &'static str {
        "MarkDirtyRequest"
    }
}

/// The requested range from a ZX_PAGER_VMO_DIRTY packet. This object must not be dropped without
/// calling either `mark_dirty` or `report_failure`.
pub type MarkDirtyRange<T> = PagerRange<T, MarkDirtyRequest>;

/// The requested range from a pager packet. This object ensures that all pager requests receive a
/// response.
pub struct PagerRange<T: PagerBacked, U: PagerRequestType> {
    range: Range<u64>,

    // A missing file indicates that a response has been sent for this range.
    file: Option<Arc<T>>,

    _request_type: PhantomData<U>,
}

impl<T: PagerBacked, U: PagerRequestType> PagerRange<T, U> {
    /// Constructs a new `PagerRange<T, U>`. `range` must be page aligned.
    fn new(range: Range<u64>, file: Arc<T>) -> Self {
        debug_assert!(
            range.start % page_size() == 0 && range.end % page_size() == 0,
            "{:?} is not page aligned",
            range
        );
        Self { range, file: Some(file), _request_type: PhantomData }
    }

    /// Splits the underlying range allowing for different parts of the range to be handled and
    /// responded to independently. See `RangeExt::split` for how splitting a range works.
    /// `split_point` must be page aligned.
    pub fn split(mut self, split_point: u64) -> (Option<Self>, Option<Self>) {
        let file = self.file.take().unwrap();
        let (left, right) = self.range.clone().split(split_point);
        let right = right.map(|range| Self::new(range, file.clone()));
        let left = left.map(|range| Self::new(range, file));
        (left, right)
    }

    /// Increases the size of the range that will be responded to. Panics if the current range is
    /// not a subset of `new_range`. `new_range` must be page aligned.
    pub fn expand(mut self, new_range: Range<u64>) -> Self {
        assert!(
            self.range.start >= new_range.start && self.range.end <= new_range.end,
            "{:?} is not a subset of {:?}",
            self.range,
            new_range
        );
        debug_assert!(
            new_range.start % page_size() == 0 && new_range.end % page_size() == 0,
            "{:?} is not page aligned",
            new_range
        );
        Self { range: new_range, file: self.file.take(), _request_type: PhantomData }
    }

    /// Returns an iterator that splits the range into ranges of `chunk_size`. If the length of the
    /// range is not a multiple of `chunk_size` then the last chunk won't be of length `chunk_size`.
    /// The returned iterator will panic if it's dropped without being fully consumed. `chunk_size`
    /// must a multiple of the page size.
    pub fn chunks(mut self, chunk_size: u64) -> PagerRangeChunksIter<T, U> {
        debug_assert!(
            chunk_size % page_size() == 0,
            "{} is not a multiple of the page size",
            chunk_size
        );
        PagerRangeChunksIter {
            start: self.range.start,
            end: self.range.end,
            chunk_size: chunk_size,
            file: self.file.take(),
            _request_type: PhantomData,
        }
    }

    #[inline]
    pub fn start(&self) -> u64 {
        self.range.start
    }

    #[inline]
    pub fn end(&self) -> u64 {
        self.range.end
    }

    #[inline]
    pub fn len(&self) -> u64 {
        self.range.end - self.range.start
    }

    #[inline]
    pub fn range(&self) -> Range<u64> {
        self.range.clone()
    }

    /// Notifies the kernel that the page request for this range has failed. See `ZX_PAGER_OP_FAIL`
    /// for more information.
    pub fn report_failure(mut self, status: zx::Status) {
        let file = self.file.take().unwrap();
        file.pager().report_failure(file.vmo(), self.range.clone(), status);
    }

    /// Test only method that will consume the PagerRange without having the send a response.
    #[cfg(test)]
    fn consume(mut self) {
        self.file.take().unwrap();
    }
}

impl<T: PagerBacked> PagerRange<T, PageInRequest> {
    /// Supplies pages to the kernel for this range. See `zx_pager_supply_pages` for more
    /// information.
    pub fn supply_pages(mut self, transfer_vmo: &zx::Vmo, transfer_offset: u64) {
        let file = self.file.take().unwrap();
        file.pager().supply_pages(file.vmo(), self.range.clone(), transfer_vmo, transfer_offset);
    }
}

impl<T: PagerBacked> PagerRange<T, MarkDirtyRequest> {
    /// Allows the kernel to dirty this range of pages. See `ZX_PAGER_OP_DIRTY` for more
    /// information.
    pub fn dirty_pages(mut self) {
        let file = self.file.take().unwrap();
        file.pager().dirty_pages(file.vmo(), self.range.clone());
    }
}

impl<T: PagerBacked, U: PagerRequestType> Drop for PagerRange<T, U> {
    fn drop(&mut self) {
        if let Some(file) = &self.file {
            let request_type = U::request_type_name();
            let range = self.range.clone();
            let key = file.pager_packet_receiver_registration().key();
            if cfg!(debug_assertions) {
                // If this object is being dropped as part of a panic then avoid panicking again.
                // Dropping pager packets when fxfs is crashing is acceptable. Panicking again would
                // only clutter the logs.
                if !std::thread::panicking() {
                    panic!(
                        "PagerRange was dropped without sending a response, \
                        request_type={request_type}, range={range:?}, key={key}",
                    );
                }
            } else {
                error!(
                    "PagerRange was dropped without sending a response, \
                    request_type={request_type}, range={range:?}, key={key}",
                );
                file.pager().report_failure(file.vmo(), range, zx::Status::BAD_STATE);
            }
        }
    }
}

/// An iterator similar to `std::slice::Chunks` which yields `PagerRange` objects.
/// `PagerRangeChunksIter` will panic if it's dropped without being fully consumed.
pub struct PagerRangeChunksIter<T: PagerBacked, U: PagerRequestType> {
    start: u64,
    end: u64,
    chunk_size: u64,
    // The file will be passed z
    file: Option<Arc<T>>,
    _request_type: PhantomData<U>,
}

impl<T: PagerBacked, U: PagerRequestType> Iterator for PagerRangeChunksIter<T, U> {
    type Item = PagerRange<T, U>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.start == self.end {
            None
        } else if self.start + self.chunk_size >= self.end {
            let next = PagerRange::new(self.start..self.end, self.file.take().unwrap());
            self.start = self.end;
            Some(next)
        } else {
            let next_end = self.start + self.chunk_size;
            let next = PagerRange::new(self.start..next_end, self.file.as_ref().unwrap().clone());
            self.start = next_end;
            Some(next)
        }
    }
}

impl<T: PagerBacked, U: PagerRequestType> Drop for PagerRangeChunksIter<T, U> {
    fn drop(&mut self) {
        if self.start != self.end {
            let request_type = U::request_type_name();
            let remaining = self.start..self.end;
            let file = self.file.take().unwrap();
            let key = file.pager_packet_receiver_registration().key();
            if cfg!(debug_assertions) {
                // If this object is being dropped as part of a panic then avoid panicking again.
                // Dropping pager packets when fxfs is crashing is acceptable. Panicking again would
                // only clutter the logs.
                if !std::thread::panicking() {
                    panic!(
                        "PagerRangeChunksIter was dropped without being fully consumed, \
                    request_type={request_type}, remaining={remaining:?}, key={key}",
                    );
                }
            } else {
                error!(
                    "PagerRangeChunksIter was dropped without being fully consumed, \
                    request_type={request_type}, remaining={remaining:?}, key={key}",
                );
                file.pager().report_failure(file.vmo(), remaining, zx::Status::BAD_STATE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, futures::channel::mpsc, futures::StreamExt};

    struct MockFile {
        vmo: zx::Vmo,
        pager_packet_receiver_registration: PagerPacketReceiverRegistration<Self>,
        pager: Arc<Pager>,
    }

    impl MockFile {
        fn new(pager: Arc<Pager>) -> Self {
            let (vmo, pager_packet_receiver_registration) = pager
                .create_vmo(page_size(), zx::VmoOptions::RESIZABLE | zx::VmoOptions::TRAP_DIRTY)
                .unwrap();
            Self { pager, vmo, pager_packet_receiver_registration }
        }
    }

    impl FxNode for MockFile {
        fn object_id(&self) -> u64 {
            unimplemented!();
        }

        fn parent(&self) -> Option<Arc<crate::directory::FxDirectory>> {
            unimplemented!();
        }

        fn set_parent(&self, _parent: Arc<crate::directory::FxDirectory>) {
            unimplemented!();
        }

        fn open_count_add_one(&self) {
            unimplemented!();
        }

        fn open_count_sub_one(self: Arc<Self>) {
            unimplemented!();
        }

        fn object_descriptor(&self) -> fxfs::object_store::ObjectDescriptor {
            unimplemented!();
        }
    }

    impl PagerBacked for MockFile {
        fn pager(&self) -> &Pager {
            &self.pager
        }

        fn pager_packet_receiver_registration(&self) -> &PagerPacketReceiverRegistration<Self> {
            &self.pager_packet_receiver_registration
        }

        fn vmo(&self) -> &zx::Vmo {
            &self.vmo
        }

        fn page_in(self: Arc<Self>, range: PageInRange<Self>) {
            let aux_vmo = zx::Vmo::create(range.len()).unwrap();
            range.supply_pages(&aux_vmo, 0);
        }

        fn mark_dirty(self: Arc<Self>, range: MarkDirtyRange<Self>) {
            range.dirty_pages();
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
        ) -> Result<buffer::Buffer<'_>, Error> {
            unimplemented!();
        }
    }

    struct OnZeroChildrenFile {
        pager: Arc<Pager>,
        vmo: zx::Vmo,
        pager_packet_receiver_registration: PagerPacketReceiverRegistration<Self>,
        sender: Mutex<mpsc::UnboundedSender<()>>,
    }

    impl OnZeroChildrenFile {
        fn new(pager: Arc<Pager>, sender: mpsc::UnboundedSender<()>) -> Self {
            let (vmo, pager_packet_receiver_registration) =
                pager.create_vmo(page_size(), zx::VmoOptions::empty()).unwrap();
            Self { pager, vmo, pager_packet_receiver_registration, sender: Mutex::new(sender) }
        }
    }

    impl FxNode for OnZeroChildrenFile {
        fn object_id(&self) -> u64 {
            unimplemented!();
        }

        fn parent(&self) -> Option<Arc<crate::directory::FxDirectory>> {
            unimplemented!();
        }

        fn set_parent(&self, _parent: Arc<crate::directory::FxDirectory>) {
            unimplemented!();
        }

        fn open_count_add_one(&self) {
            unimplemented!();
        }

        fn open_count_sub_one(self: Arc<Self>) {
            unimplemented!();
        }

        fn object_descriptor(&self) -> fxfs::object_store::ObjectDescriptor {
            unimplemented!();
        }
    }

    impl PagerBacked for OnZeroChildrenFile {
        fn pager(&self) -> &Pager {
            &self.pager
        }

        fn pager_packet_receiver_registration(&self) -> &PagerPacketReceiverRegistration<Self> {
            &self.pager_packet_receiver_registration
        }

        fn vmo(&self) -> &zx::Vmo {
            &self.vmo
        }

        fn page_in(self: Arc<Self>, _range: PageInRange<Self>) {
            unreachable!();
        }

        fn mark_dirty(self: Arc<Self>, _range: MarkDirtyRange<Self>) {
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
        ) -> Result<buffer::Buffer<'_>, Error> {
            unreachable!();
        }
    }

    #[fuchsia::test(threads = 2)]
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

    #[fuchsia::test(threads = 2)]
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

    #[fuchsia::test(threads = 2)]
    async fn test_status_code_mapping() {
        struct StatusCodeFile {
            vmo: zx::Vmo,
            pager: Arc<Pager>,
            status_code: Mutex<zx::Status>,
            pager_packet_receiver_registration: PagerPacketReceiverRegistration<Self>,
        }

        impl FxNode for StatusCodeFile {
            fn object_id(&self) -> u64 {
                unimplemented!();
            }

            fn parent(&self) -> Option<Arc<crate::directory::FxDirectory>> {
                unimplemented!();
            }

            fn set_parent(&self, _parent: Arc<crate::directory::FxDirectory>) {
                unimplemented!();
            }

            fn open_count_add_one(&self) {
                unimplemented!();
            }

            fn open_count_sub_one(self: Arc<Self>) {
                unimplemented!();
            }

            fn object_descriptor(&self) -> fxfs::object_store::ObjectDescriptor {
                unimplemented!();
            }
        }

        impl PagerBacked for StatusCodeFile {
            fn pager(&self) -> &Pager {
                &self.pager
            }

            fn pager_packet_receiver_registration(&self) -> &PagerPacketReceiverRegistration<Self> {
                &self.pager_packet_receiver_registration
            }

            fn vmo(&self) -> &zx::Vmo {
                &self.vmo
            }

            fn page_in(self: Arc<Self>, range: PageInRange<Self>) {
                range.report_failure(*self.status_code.lock().unwrap());
            }

            fn mark_dirty(self: Arc<Self>, _range: MarkDirtyRange<Self>) {
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
            ) -> Result<buffer::Buffer<'_>, Error> {
                unreachable!();
            }
        }

        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope.clone()).unwrap());
        let (vmo, pager_packet_receiver_registration) =
            pager.create_vmo(page_size(), zx::VmoOptions::empty()).unwrap();
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

    #[fuchsia::test(threads = 2)]
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

    #[fuchsia::test(threads = 2)]
    async fn test_query_dirty_ranges() {
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope.clone()).unwrap());
        let file = Arc::new(MockFile::new(pager.clone()));
        pager.register_file(&file);

        let page_size = page_size();
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

    #[fuchsia::test]
    async fn test_pager_range_chunks_iter_chunks() {
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope).unwrap());
        let file = Arc::new(MockFile::new(pager));

        let pager_range = PageInRange::new(0..page_size() * 5, file);
        let ranges: Vec<Range<u64>> = pager_range
            .chunks(page_size() * 2)
            .map(|pager_range| {
                let range = pager_range.range();
                pager_range.consume();
                range
            })
            .collect();
        assert_eq!(
            ranges,
            [
                0..page_size() * 2,
                page_size() * 2..page_size() * 4,
                page_size() * 4..page_size() * 5
            ]
        );
    }

    #[fuchsia::test]
    async fn test_pager_range_split() {
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope).unwrap());
        let file = Arc::new(MockFile::new(pager));

        let pager_range = PageInRange::new(0..page_size() * 10, file);
        let (left, right) = pager_range.split(page_size() * 5);
        let (left, right) = (left.unwrap(), right.unwrap());
        assert_eq!(left.range(), 0..page_size() * 5);
        assert_eq!(right.range(), page_size() * 5..page_size() * 10);

        left.consume();
        right.consume();
    }

    #[fuchsia::test]
    #[should_panic(expected = "0..8192 is not a subset of 0..4096")]
    async fn test_pager_range_bad_expand_panics() {
        let scope = ExecutionScope::new();
        let pager = Arc::new(Pager::new(scope).unwrap());
        let file = Arc::new(MockFile::new(pager));

        let pager_range = PageInRange::new(0..page_size() * 2, file);
        pager_range.expand(0..page_size()).consume();
    }

    struct PagerRangeTestFile {
        vmo: zx::Vmo,
        pager_packet_receiver_registration: PagerPacketReceiverRegistration<Self>,
        pager: Pager,
        page_in_fn: Box<dyn Fn(PageInRange<Self>) + Send + Sync + 'static>,
        mark_dirty_fn: Box<dyn Fn(MarkDirtyRange<Self>) + Send + Sync + 'static>,
    }

    impl PagerRangeTestFile {
        fn new<
            F1: Fn(PageInRange<Self>) + Send + Sync + 'static,
            F2: Fn(MarkDirtyRange<Self>) + Send + Sync + 'static,
        >(
            page_in_fn: F1,
            mark_dirty_fn: F2,
        ) -> Arc<Self> {
            let pager = Pager::new(ExecutionScope::new()).unwrap();
            let (vmo, pager_packet_receiver_registration) =
                pager.create_vmo(page_size() * 2, zx::VmoOptions::TRAP_DIRTY).unwrap();
            let this = Arc::new(Self {
                vmo,
                pager_packet_receiver_registration,
                pager,
                page_in_fn: Box::new(page_in_fn),
                mark_dirty_fn: Box::new(mark_dirty_fn),
            });
            this.pager.register_file(&this);
            this
        }
    }

    impl FxNode for PagerRangeTestFile {
        fn object_id(&self) -> u64 {
            1
        }

        fn parent(&self) -> Option<Arc<crate::directory::FxDirectory>> {
            unimplemented!()
        }

        fn set_parent(&self, _parent: Arc<crate::directory::FxDirectory>) {
            unimplemented!()
        }

        fn open_count_add_one(&self) {
            unimplemented!()
        }

        fn open_count_sub_one(self: Arc<Self>) {
            unimplemented!()
        }

        fn object_descriptor(&self) -> fxfs::object_store::ObjectDescriptor {
            unimplemented!()
        }
    }

    impl PagerBacked for PagerRangeTestFile {
        fn pager(&self) -> &Pager {
            &self.pager
        }

        fn pager_packet_receiver_registration(&self) -> &PagerPacketReceiverRegistration<Self> {
            &self.pager_packet_receiver_registration
        }

        fn vmo(&self) -> &zx::Vmo {
            &self.vmo
        }

        fn page_in(self: Arc<Self>, range: PageInRange<Self>) {
            (self.page_in_fn)(range)
        }

        fn mark_dirty(self: Arc<Self>, range: MarkDirtyRange<Self>) {
            (self.mark_dirty_fn)(range)
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
            _range: std::ops::Range<u64>,
        ) -> Result<buffer::Buffer<'_>, Error> {
            unimplemented!();
        }
    }

    fn real_supply_pages(range: PageInRange<PagerRangeTestFile>) {
        let aux_vmo = zx::Vmo::create(range.len()).unwrap();
        range.supply_pages(&aux_vmo, 0);
    }

    fn real_mark_dirty(range: MarkDirtyRange<PagerRangeTestFile>) {
        range.dirty_pages();
    }

    #[fuchsia::test(threads = 2)]
    async fn test_page_in_range_supply_pages() {
        let file = PagerRangeTestFile::new(real_supply_pages, real_mark_dirty);

        let mut data = vec![0; 20];
        file.vmo.read(&mut data, 0).unwrap();
    }

    #[fuchsia::test(threads = 2)]
    async fn test_page_in_range_report_failure() {
        let file = PagerRangeTestFile::new(
            |range| {
                range.report_failure(zx::Status::IO_DATA_INTEGRITY);
            },
            real_mark_dirty,
        );

        let mut data = vec![0; 20];
        let err = file.vmo.read(&mut data, 0).unwrap_err();
        assert_eq!(err, zx::Status::IO_DATA_INTEGRITY);
    }

    #[cfg(debug_assertions)]
    #[fuchsia::test(threads = 2)]
    #[should_panic(expected = "PagerRange was dropped without sending a response")]
    async fn test_page_in_range_dropped() {
        let file = PagerRangeTestFile::new(|_| {}, real_mark_dirty);

        let mut data = vec![0; 20];
        file.vmo.read(&mut data, 0).unwrap_err();
    }

    #[cfg(not(debug_assertions))]
    #[fuchsia::test(threads = 2)]
    async fn test_page_in_range_dropped() {
        let file = PagerRangeTestFile::new(|_| {}, real_mark_dirty);

        let mut data = vec![0; 20];
        let err = file.vmo.read(&mut data, 0).unwrap_err();
        assert_eq!(err, zx::Status::BAD_STATE);
    }

    #[fuchsia::test(threads = 2)]
    async fn test_mark_dirty_range_dirty_pages() {
        let file = PagerRangeTestFile::new(real_supply_pages, real_mark_dirty);

        let data = vec![5; 20];
        file.vmo.write(&data, 0).unwrap();
    }

    #[fuchsia::test(threads = 2)]
    async fn test_mark_dirty_range_report_failure() {
        let file = PagerRangeTestFile::new(real_supply_pages, |range| {
            range.report_failure(zx::Status::IO_DATA_INTEGRITY);
        });

        let data = vec![5; 20];
        let err = file.vmo.write(&data, 0).unwrap_err();
        assert_eq!(err, zx::Status::IO_DATA_INTEGRITY);
    }

    #[cfg(debug_assertions)]
    #[fuchsia::test(threads = 2)]
    #[should_panic(expected = "PagerRange was dropped without sending a response")]
    async fn test_mark_dirty_range_dropped() {
        let file = PagerRangeTestFile::new(real_supply_pages, |_| {});

        let data = vec![5; 20];
        file.vmo.write(&data, 0).unwrap_err();
    }

    #[cfg(not(debug_assertions))]
    #[fuchsia::test(threads = 2)]
    async fn test_mark_dirty_range_dropped() {
        let file = PagerRangeTestFile::new(real_supply_pages, |_| {});

        let data = vec![5; 20];
        let err = file.vmo.write(&data, 0).unwrap_err();
        assert_eq!(err, zx::Status::BAD_STATE);
    }

    #[fuchsia::test(threads = 2)]
    async fn test_pager_range_chunks_iter_consumed() {
        let file = PagerRangeTestFile::new(
            |range| {
                let aux_vmo = zx::Vmo::create(page_size()).unwrap();
                range.expand(0..page_size() * 2).chunks(page_size()).for_each(|range| {
                    range.supply_pages(&aux_vmo, 0);
                });
            },
            real_mark_dirty,
        );

        let mut data = vec![0; 20];
        file.vmo.read(&mut data, 0).unwrap();
    }

    fn partial_supply_pages(range: PageInRange<PagerRangeTestFile>) {
        let aux_vmo = zx::Vmo::create(page_size()).unwrap();
        // Expand the range to 2 pages and only supply the first page, dropping the iterator without
        // fully consuming it.
        range.expand(0..page_size() * 2).chunks(page_size()).take(1).for_each(|range| {
            range.supply_pages(&aux_vmo, 0);
        });
    }

    #[cfg(debug_assertions)]
    #[fuchsia::test(threads = 2)]
    #[should_panic(expected = "PagerRangeChunksIter was dropped without being fully consumed")]
    async fn test_pager_range_chunks_iter_dropped() {
        let file = PagerRangeTestFile::new(partial_supply_pages, real_mark_dirty);

        let mut data = vec![0; 20];
        // Ask for the 2nd page. The range will be expanded to the first 2 pages. The first page
        // will succeed and the second page will be dropped.
        file.vmo.read(&mut data, page_size()).unwrap_err();
    }

    #[cfg(not(debug_assertions))]
    #[fuchsia::test(threads = 2)]
    async fn test_pager_range_chunks_iter_dropped() {
        let file = PagerRangeTestFile::new(partial_supply_pages, real_mark_dirty);

        let mut data = vec![0; 20];
        // Ask for the 2nd page. The range will be expanded to the first 2 pages. The first page
        // will succeed and the second page will be dropped.
        let err = file.vmo.read(&mut data, page_size()).unwrap_err();
        assert_eq!(err, zx::Status::BAD_STATE);
    }
}
