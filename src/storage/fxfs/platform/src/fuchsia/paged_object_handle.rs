// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fuchsia::pager::{
    MarkDirtyRange, Pager, PagerBacked, PagerVmoStatsOptions, VmoDirtyRange,
};
use crate::fuchsia::volume::FxVolume;
use anyhow::{ensure, Context, Error};
use fidl_fuchsia_io as fio;
use fxfs::errors::FxfsError;
use fxfs::filesystem::MAX_FILE_SIZE;
use fxfs::log::*;
use fxfs::object_handle::{ObjectHandle, ObjectProperties, ReadObjectHandle};
use fxfs::object_store::allocator::{Allocator, Reservation, ReservationOwner};
use fxfs::object_store::transaction::{
    lock_keys, LockKey, Options, Transaction, TRANSACTION_METADATA_MAX_AMOUNT,
};
use fxfs::object_store::{DataObjectHandle, ObjectStore, RangeType, StoreObjectHandle, Timestamp};
use fxfs::range::RangeExt;
use fxfs::round::{how_many, round_up};
use scopeguard::ScopeGuard;
use std::future::Future;
use std::ops::Range;
use std::sync::{Arc, Mutex};
use storage_device::buffer::{Buffer, BufferFuture};
use vfs::temp_clone::{unblock, TempClonable};

/// How much data each sync transaction in a given flush will cover.
const FLUSH_BATCH_SIZE: u64 = 524_288;

/// An expanding write will: mark a page as dirty, write to the page, and then update the content
/// size. If a flush is triggered during an expanding write then query_dirty_ranges may return pages
/// that have been marked dirty but are beyond the stream size. Those extra pages can't be cleaned
/// during the flush and will have to be cleaned in a later flush. The initial flush will consume
/// the transaction metadata space that the extra pages were supposed to be part of leaving no
/// transaction metadata space for the extra pages in the next flush if no additional pages are
/// dirtied. `SPARE_SIZE` is extra metadata space that gets reserved be able to flush the extra
/// pages if this situation occurs.
const SPARE_SIZE: u64 = TRANSACTION_METADATA_MAX_AMOUNT;

pub struct PagedObjectHandle {
    inner: Mutex<Inner>,
    vmo: TempClonable<zx::Vmo>,
    handle: DataObjectHandle<FxVolume>,
}
#[derive(Debug)]
struct Inner {
    dirty_crtime: DirtyTimestamp,
    dirty_mtime: DirtyTimestamp,

    /// The number of pages that have been marked dirty by the kernel and need to be cleaned.
    dirty_page_count: u64,

    /// The amount of extra space currently reserved. See `SPARE_SIZE`.
    spare: u64,

    /// Stores whether the file needs to be shrunk or trimmed during the next flush.
    pending_shrink: PendingShrink,

    /// This bit is set at the top of enable_verity(). Once this bit is set, all future calls to
    /// mark_dirty() should fail. This ensures that the contents of the file do not change while
    /// the merkle tree is being computed or thereon after.
    read_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PendingShrink {
    None,

    /// The file needs to be shrunk during the next flush. After shrinking the file, the file may
    /// then also need to be trimmed.
    ShrinkTo(u64),

    /// The file needs to be trimmed during the next flush.
    NeedsTrim,
}

// DirtyTimestamp tracks a dirty timestamp and handles flushing. Whilst we're flushing, we need to
// hang on to the timestamp in case anything queries it, but once we've finished, we can discard it
// so long as it hasn't been written again.
#[derive(Clone, Copy, Debug)]
enum DirtyTimestamp {
    None,
    Some(Timestamp),
    PendingFlush(Timestamp),
}

impl DirtyTimestamp {
    // If we have a timestamp, move to the PendingFlush state.
    fn begin_flush(&mut self, update_to_now: bool) -> Option<Timestamp> {
        if update_to_now {
            let now = Timestamp::now();
            *self = DirtyTimestamp::PendingFlush(now);
            Some(now)
        } else {
            match self {
                DirtyTimestamp::None => None,
                DirtyTimestamp::Some(t) => {
                    let t = *t;
                    *self = DirtyTimestamp::PendingFlush(t);
                    Some(t)
                }
                DirtyTimestamp::PendingFlush(t) => Some(*t),
            }
        }
    }

    // We finished a flush, so discard it if no further update was made.
    fn end_flush(&mut self) {
        if let DirtyTimestamp::PendingFlush(_) = self {
            *self = DirtyTimestamp::None;
        }
    }

    fn timestamp(&self) -> Option<Timestamp> {
        match self {
            DirtyTimestamp::None => None,
            DirtyTimestamp::Some(t) => Some(*t),
            DirtyTimestamp::PendingFlush(t) => Some(*t),
        }
    }

    fn needs_flush(&self) -> bool {
        !matches!(self, DirtyTimestamp::None)
    }
}

impl std::convert::From<Option<Timestamp>> for DirtyTimestamp {
    fn from(value: Option<Timestamp>) -> Self {
        if let Some(t) = value {
            DirtyTimestamp::Some(t)
        } else {
            DirtyTimestamp::None
        }
    }
}

/// Returns the amount of space that should be reserved to be able to flush `page_count` pages.
fn reservation_needed(page_count: u64) -> u64 {
    let page_size = zx::system_get_page_size() as u64;
    let pages_per_transaction = FLUSH_BATCH_SIZE / page_size;
    let transaction_count = how_many(page_count, pages_per_transaction);
    transaction_count * TRANSACTION_METADATA_MAX_AMOUNT + page_count * page_size
}

/// Returns the number of pages spanned by `range`. `range` must be page aligned.
fn page_count(range: Range<u64>) -> u64 {
    let page_size = zx::system_get_page_size() as u64;
    debug_assert!(range.start <= range.end);
    debug_assert!(range.start % page_size == 0);
    debug_assert!(range.end % page_size == 0);
    (range.end - range.start) / page_size
}

/// Drops `guard` without running the callback.
fn dismiss_scopeguard<T, U: std::ops::FnOnce(T), S: scopeguard::Strategy>(
    guard: ScopeGuard<T, U, S>,
) {
    ScopeGuard::into_inner(guard);
}

impl Inner {
    fn reservation(&self) -> u64 {
        reservation_needed(self.dirty_page_count) + self.spare
    }

    /// Takes all the dirty pages and returns a (<count of dirty pages>, <reservation>).
    fn take(&mut self, allocator: Arc<Allocator>, store_object_id: u64) -> (u64, Reservation) {
        let reservation = allocator.reserve_at_most(Some(store_object_id), 0);
        reservation.add(self.reservation());
        self.spare = 0;
        (std::mem::take(&mut self.dirty_page_count), reservation)
    }

    /// Takes all the dirty pages and adds to the reservation.  Returns the number of dirty pages.
    fn move_to(&mut self, reservation: &Reservation) -> u64 {
        reservation.add(self.reservation());
        self.spare = 0;
        std::mem::take(&mut self.dirty_page_count)
    }

    // Put back some dirty pages taking from reservation as required.
    fn put_back(&mut self, count: u64, reservation: &Reservation) {
        if count > 0 {
            let before = self.reservation();
            self.dirty_page_count += count;
            let needed = reservation_needed(self.dirty_page_count);
            self.spare = std::cmp::min(reservation.amount() + before - needed, SPARE_SIZE);
            reservation.forget_some(needed + self.spare - before);
        }
    }

    fn end_flush(&mut self) {
        self.dirty_mtime.end_flush();
        self.dirty_crtime.end_flush();
    }
}

impl PagedObjectHandle {
    pub fn new(handle: DataObjectHandle<FxVolume>, vmo: zx::Vmo) -> Self {
        let verified_file = handle.verified_file();
        Self {
            vmo: TempClonable::new(vmo),
            handle,
            inner: Mutex::new(Inner {
                dirty_crtime: DirtyTimestamp::None,
                dirty_mtime: DirtyTimestamp::None,
                dirty_page_count: 0,
                spare: 0,
                pending_shrink: PendingShrink::None,
                read_only: verified_file,
            }),
        }
    }

    pub fn owner(&self) -> &Arc<FxVolume> {
        self.handle.owner()
    }

    pub fn store(&self) -> &ObjectStore {
        self.handle.store()
    }

    pub fn vmo(&self) -> &zx::Vmo {
        &self.vmo
    }

    pub fn pager(&self) -> &Pager {
        self.owner().pager()
    }

    pub fn set_read_only(&self) {
        self.inner.lock().unwrap().read_only = true
    }

    pub fn get_size(&self) -> u64 {
        self.vmo.get_stream_size().unwrap()
    }

    pub fn pre_fetch_keys(&self) -> Option<impl Future<Output = ()>> {
        self.handle.pre_fetch_keys()
    }

    async fn new_transaction<'a>(
        &self,
        reservation: Option<&'a Reservation>,
    ) -> Result<Transaction<'a>, Error> {
        self.store()
            .filesystem()
            .new_transaction(
                lock_keys![LockKey::object(
                    self.handle.store().store_object_id(),
                    self.handle.object_id()
                )],
                Options {
                    skip_journal_checks: false,
                    borrow_metadata_space: reservation.is_none(),
                    allocator_reservation: reservation,
                    ..Default::default()
                },
            )
            .await
    }

    fn allocator(&self) -> Arc<Allocator> {
        self.store().filesystem().allocator()
    }

    pub fn uncached_handle(&self) -> &DataObjectHandle<FxVolume> {
        &self.handle
    }

    pub fn uncached_size(&self) -> u64 {
        self.handle.get_size()
    }

    pub fn store_handle(&self) -> &StoreObjectHandle<FxVolume> {
        &*self.handle
    }

    pub async fn read_uncached(&self, range: std::ops::Range<u64>) -> Result<Buffer<'_>, Error> {
        let mut buffer = self.handle.allocate_buffer((range.end - range.start) as usize).await;
        let read = self.handle.read(range.start, buffer.as_mut()).await?;
        buffer.as_mut_slice()[read..].fill(0);
        Ok(buffer)
    }

    pub fn mark_dirty<T: PagerBacked>(
        &self,
        page_range: MarkDirtyRange<T>,
    ) -> Result<(), zx::Status> {
        let mut inner = self.inner.lock().unwrap();
        if inner.read_only {
            // Enable-verity has already been called on this file.
            page_range.report_failure(zx::Status::BAD_STATE);
            return Err(zx::Status::BAD_STATE);
        }
        let mut pages_added = 0;
        for subrange in self.handle.overwrite_ranges_overlap(page_range.range()) {
            // Check the overwrite ranges we have recorded for this file. We only add to the
            // reservation if the range is not one of our overwrite ranges, since overwrite ranges
            // are already allocated.
            if let RangeType::Cow(cow_range) = subrange {
                pages_added += page_count(cow_range);
            }
        }
        let new_inner = Inner {
            dirty_page_count: inner.dirty_page_count + pages_added,
            spare: SPARE_SIZE,
            ..*inner
        };
        let previous_reservation = inner.reservation();
        let new_reservation = new_inner.reservation();
        let reservation_delta = new_reservation - previous_reservation;
        // The reserved amount will never decrease but might be the same.
        if reservation_delta > 0 {
            match self.allocator().reserve(Some(self.store().store_object_id()), reservation_delta)
            {
                Some(reservation) => {
                    // `PagedObjectHandle` doesn't hold onto a `Reservation` object for tracking
                    // reservations. The amount of space reserved by a `PagedObjectHandle` should
                    // always be derivable from `Inner`.
                    reservation.forget();
                }
                None => {
                    page_range.report_failure(zx::Status::NO_SPACE);
                    return Err(zx::Status::NO_SPACE);
                }
            }
        }
        *inner = new_inner;
        page_range.dirty_pages();
        Ok(())
    }

    /// Queries the VMO to see if it was modified since the last time this function was called.
    fn was_file_modified_since_last_call(&self) -> Result<bool, zx::Status> {
        let stats =
            self.pager().query_vmo_stats(self.vmo(), PagerVmoStatsOptions::RESET_VMO_STATS)?;
        Ok(stats.was_vmo_modified())
    }

    /// Calls `query_dirty_ranges` to collect the ranges of the VMO that need to be flushed.
    fn collect_modified_ranges(&self) -> Result<Vec<VmoDirtyRange>, Error> {
        let mut modified_ranges: Vec<VmoDirtyRange> = Vec::new();
        let vmo = self.vmo();
        let pager = self.pager();

        // Whilst it's tempting to only collect ranges within 0..content_size, we need to collect
        // all the ranges so we can count up how many pages we're not going to flush, and then
        // make sure we return them so that we keep sufficient space reserved.
        let vmo_size = vmo.get_size()?;

        // `query_dirty_ranges` includes both dirty ranges and zero ranges. If there are no zero
        // pages and all of the dirty pages are consecutive then we'll receive only one range back
        // for all of the dirty pages. On the other end, there could be alternating zero and dirty
        // pages resulting in two times the number dirty pages in ranges. Also, since flushing
        // doesn't block mark_dirty, the number of ranges may change as they are being queried. 16
        // ranges was chosen as the initial buffer size to avoid wastefully using memory while also
        // being sufficient for common file usage patterns.
        let mut remaining = 16;
        let mut offset = 0;
        let mut total_received = 0;
        loop {
            modified_ranges.resize(total_received + remaining, VmoDirtyRange::default());
            let actual;
            (actual, remaining) = pager
                .query_dirty_ranges(vmo, offset..vmo_size, &mut modified_ranges[total_received..])
                .context("query_dirty_ranges failed")?;
            total_received += actual;
            // If fewer ranges were received than asked for then drop the extra allocated ranges.
            modified_ranges.resize(total_received, VmoDirtyRange::default());
            if actual == 0 {
                break;
            }
            let last = modified_ranges.last().unwrap();
            offset = last.range().end;
            if remaining == 0 {
                break;
            }
        }
        Ok(modified_ranges)
    }

    /// Queries for the ranges that need to be flushed and splits the ranges into batches that will
    /// each fit into a single transaction.
    fn collect_flush_batches(
        &self,
        content_size: u64,
    ) -> Result<(Vec<FlushBatch>, u64, u64), Error> {
        let page_aligned_content_size = round_up(content_size, zx::system_get_page_size()).unwrap();
        let modified_ranges =
            self.collect_modified_ranges().context("collect_modified_ranges failed")?;

        let mut flush_batches = FlushBatches::default();
        let mut last_end = 0;
        for modified_range in modified_ranges {
            // Skip ranges entirely past the stream size.  It might be tempting to consider
            // flushing the range anyway and making up some value for stream size, but that's not
            // safe because the pages will be zeroed before they are written to and it would be
            // wrong to write zeroed data.
            let (range, past_content_size_page_range) =
                modified_range.range().split(page_aligned_content_size);

            if let Some(past_content_size_page_range) = past_content_size_page_range {
                // For now, any data past the end of the content size won't be pre-allocated, so we
                // don't need to consider it when calculating the reservation size. This might
                // change if we support fallocate with the KEEP_SIZE mode which allows for
                // allocations past the end of a file. We also won't be in the middle of an
                // allocation that may have been split into multiple transactions because allocate
                // takes the flush lock.
                if !modified_range.is_zero_range() {
                    // If the range is not zero then space should have been reserved for it that
                    // should continue to be reserved after this flush.
                    flush_batches.skip_range(past_content_size_page_range);
                }
            }

            if let Some(range) = range {
                // Ranges must be returned in order.
                assert!(range.start >= last_end);
                last_end = range.end;
                for range_chunk in self.uncached_handle().overwrite_ranges_overlap(range) {
                    let (range, mode) = match range_chunk {
                        RangeType::Cow(range) => (
                            range,
                            if modified_range.is_zero_range() {
                                BatchMode::Zero
                            } else {
                                BatchMode::Cow
                            },
                        ),
                        RangeType::Overwrite(range) => (range, BatchMode::Overwrite),
                    };
                    flush_batches.add_range(range, mode);
                }
            }
        }

        Ok(flush_batches.consume())
    }

    async fn add_metadata_to_transaction<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        content_size: Option<u64>,
        crtime: Option<Timestamp>,
        mtime: Option<Timestamp>,
        ctime: Option<Timestamp>,
    ) -> Result<(), Error> {
        if let Some(content_size) = content_size {
            self.handle.txn_update_size(transaction, content_size).await?;
        }
        let attributes = fio::MutableNodeAttributes {
            creation_time: crtime.map(|t| t.as_nanos()),
            modification_time: mtime.map(|t| t.as_nanos()),
            ..Default::default()
        };
        self.handle
            .update_attributes(transaction, Some(&attributes), ctime)
            .await
            .context("update_attributes failed")?;
        Ok(())
    }

    /// Flushes only the metadata of the file by borrowing metadata space.
    async fn flush_metadata(
        &self,
        content_size: u64,
        previous_content_size: u64,
        crtime: Option<Timestamp>,
        mtime: Option<Timestamp>,
    ) -> Result<(), Error> {
        let mut transaction = self.new_transaction(None).await?;
        self.add_metadata_to_transaction(
            &mut transaction,
            if content_size == previous_content_size { None } else { Some(content_size) },
            crtime,
            mtime.clone(),
            mtime,
        )
        .await?;
        transaction.commit().await.context("Failed to commit transaction")?;
        Ok(())
    }

    async fn flush_data<T: FnOnce(u64)>(
        &self,
        reservation: &Reservation,
        mut reservation_guard: ScopeGuard<u64, T>,
        mut content_size: u64,
        mut previous_content_size: u64,
        crtime: Option<Timestamp>,
        mtime: Option<Timestamp>,
        flush_batches: Vec<FlushBatch>,
    ) -> Result<(), Error> {
        let pager = self.pager();
        let vmo = self.vmo();

        let last_batch_index = flush_batches.len() - 1;
        for (i, batch) in flush_batches.into_iter().enumerate() {
            let first_batch = i == 0;
            let last_batch = i == last_batch_index;

            let mut transaction = match batch.mode {
                BatchMode::Zero => self.new_transaction(None).await?,
                BatchMode::Cow => self.new_transaction(Some(&reservation)).await?,
                BatchMode::Overwrite => self.new_transaction(None).await?,
            };
            batch.writeback_begin(vmo, pager);

            let size = if last_batch {
                if batch.end() > content_size {
                    // Now that we've called writeback_begin, get the stream size again.  If the
                    // stream size has increased (it can't decrease because we hold a lock on
                    // truncation), it's possible that it grew before we called writeback_begin in
                    // which case, the kernel won't mark the tail page dirty again so we must
                    // increase the stream size, but no further than the end of the tail page.
                    let new_content_size =
                        self.vmo().get_stream_size().context("get_stream_size failed")?;

                    assert!(new_content_size >= content_size);

                    content_size = std::cmp::min(new_content_size, batch.end())
                }
                Some(content_size)
            } else if batch.end() > previous_content_size {
                Some(batch.end())
            } else {
                None
            }
            .filter(|s| {
                let changed = *s != previous_content_size;
                previous_content_size = *s;
                changed
            });

            self.add_metadata_to_transaction(
                &mut transaction,
                size,
                if first_batch { crtime } else { None },
                if first_batch { mtime.clone() } else { None },
                if first_batch { mtime } else { None },
            )
            .await?;

            batch
                .add_to_transaction(&mut transaction, &self.vmo, &self.handle, content_size)
                .await
                .context("batch add_to_transaction failed")?;
            transaction.commit().await.context("Failed to commit transaction")?;
            if batch.mode == BatchMode::Cow {
                *reservation_guard -= batch.page_count();
            }
            if first_batch {
                self.inner.lock().unwrap().end_flush();
            }

            batch.writeback_end(vmo, pager);
            if batch.mode == BatchMode::Cow {
                self.owner().report_pager_clean(batch.dirty_byte_count);
            }
        }

        // Before releasing the reservation, mark those pages as cleaned, since they weren't used.
        self.owner().report_pager_clean(*reservation_guard);
        dismiss_scopeguard(reservation_guard);

        Ok(())
    }

    async fn flush_impl(&self) -> Result<(), Error> {
        if !self.needs_flush() {
            return Ok(());
        }

        let store = self.handle.store();
        let fs = store.filesystem();
        // If the VMO is shrunk between getting the VMO's size and calling query_dirty_ranges or
        // reading the cached data then the flush could fail. This lock is held to prevent the file
        // from shrinking while it's being flushed.
        // NB: FxFile.open_count_sub_one_and_maybe_flush relies on this lock being taken to make
        // sure any flushes are done before it adds a file tombstone if the file is going to be
        // purged. If this lock key changes, it should change there as well.
        let keys = lock_keys![LockKey::truncate(store.store_object_id(), self.handle.object_id())];
        let _truncate_guard = fs.lock_manager().write_lock(keys).await;

        self.handle.owner().pager().page_in_barrier().await;

        let pending_shrink = self.inner.lock().unwrap().pending_shrink;
        if let PendingShrink::ShrinkTo(size) = pending_shrink {
            let needs_trim = self.shrink_file(size).await.context("Failed to shrink file")?;
            self.inner.lock().unwrap().pending_shrink =
                if needs_trim { PendingShrink::NeedsTrim } else { PendingShrink::None };
        }

        let pending_shrink = self.inner.lock().unwrap().pending_shrink;
        if let PendingShrink::NeedsTrim = pending_shrink {
            self.store().trim(self.object_id()).await.context("Failed to trim file")?;
            self.inner.lock().unwrap().pending_shrink = PendingShrink::None;
        }

        // If the file had several dirty pages and then was truncated to before those dirty pages
        // then we'll still have space reserved that is no longer needed and should be released as
        // part of this flush.
        //
        // If `reservation` and `dirty_pages` were pulled out of `inner` after calling
        // `query_dirty_ranges` then we wouldn't be able to tell the difference between pages there
        // dirtied between those 2 operations and dirty pages that were made irrelevant by the
        // truncate.
        let (mtime, crtime, (dirty_pages, reservation)) = {
            let mut inner = self.inner.lock().unwrap();
            (
                inner.dirty_mtime.begin_flush(self.was_file_modified_since_last_call()?),
                inner.dirty_crtime.begin_flush(false),
                inner.take(self.allocator(), self.store().store_object_id()),
            )
        };

        let mut reservation_guard = scopeguard::guard(dirty_pages, |dirty_pages| {
            self.inner.lock().unwrap().put_back(dirty_pages, &reservation);
        });

        let content_size = self.vmo().get_stream_size().context("get_stream_size failed")?;
        let previous_content_size = self.handle.get_size();
        let (flush_batches, required_reserved_pages, mut pages_not_flushed) =
            self.collect_flush_batches(content_size)?;

        // If pages were dirtied between getting the reservation and collecting the dirty ranges
        // then we might need to update the reservation.
        if required_reserved_pages > dirty_pages {
            // This potentially takes more reservation than might be necessary.  We could perhaps
            // optimize this to take only what might be required.
            let new_dirty_pages = self.inner.lock().unwrap().move_to(&reservation);

            // Make sure we account for pages we might not flush to ensure we keep them reserved.
            pages_not_flushed = dirty_pages + new_dirty_pages - required_reserved_pages;

            // Make sure we return the new dirty pages on failure.
            *reservation_guard += new_dirty_pages;

            assert!(
                reservation.amount() >= reservation_needed(required_reserved_pages),
                "reservation: {}, needed: {}, dirty_pages: {}, pages_to_flush: {}",
                reservation.amount(),
                reservation_needed(required_reserved_pages),
                dirty_pages,
                required_reserved_pages
            );
        } else {
            // The reservation we have is sufficient for pages_to_flush, but it might not be enough
            // for pages_not_flushed as well.
            pages_not_flushed =
                std::cmp::min(dirty_pages - required_reserved_pages, pages_not_flushed);
        }

        if flush_batches.is_empty() {
            self.flush_metadata(content_size, previous_content_size, crtime, mtime).await?;
            dismiss_scopeguard(reservation_guard);
            self.inner.lock().unwrap().end_flush();
        } else {
            self.flush_data(
                &reservation,
                reservation_guard,
                content_size,
                previous_content_size,
                crtime,
                mtime,
                flush_batches,
            )
            .await?
        }

        let mut inner = self.inner.lock().unwrap();
        inner.put_back(pages_not_flushed, &reservation);

        Ok(())
    }

    pub async fn flush(&self) -> Result<(), Error> {
        match self.flush_impl().await {
            Ok(()) => Ok(()),
            Err(error) => {
                error!(?error, "Failed to flush");
                Err(error)
            }
        }
    }

    /// Returns true if the file still needs to be trimmed.
    async fn shrink_file(&self, new_size: u64) -> Result<bool, Error> {
        let mut transaction = self.new_transaction(None).await?;

        let needs_trim = self.handle.shrink(&mut transaction, new_size).await?.0;

        let (mtime, crtime) = {
            let mut inner = self.inner.lock().unwrap();
            (
                inner.dirty_mtime.begin_flush(self.was_file_modified_since_last_call()?),
                inner.dirty_crtime.begin_flush(false),
            )
        };

        let attributes = fio::MutableNodeAttributes {
            creation_time: crtime.map(|t| t.as_nanos()),
            modification_time: mtime.map(|t| t.as_nanos()),
            ..Default::default()
        };
        // Shrinking the file should also update `change_time` (it'd be the same value as the
        // modification time).
        self.handle
            .update_attributes(&mut transaction, Some(&attributes), mtime)
            .await
            .context("update_attributes failed")?;
        transaction.commit().await.context("Failed to commit transaction")?;
        self.inner.lock().unwrap().end_flush();

        Ok(needs_trim)
    }

    pub async fn truncate(&self, new_size: u64) -> Result<(), Error> {
        ensure!(new_size <= MAX_FILE_SIZE, FxfsError::InvalidArgs);
        let store = self.handle.store();
        let fs = store.filesystem();
        let keys = lock_keys![LockKey::truncate(store.store_object_id(), self.handle.object_id())];
        let _truncate_guard = fs.lock_manager().write_lock(keys).await;

        let vmo = self.vmo.temp_clone();
        // This unblock is to break an executor ordering deadlock situation. Vmo::set_stream_size()
        // may trigger a blocking call back into Fxfs on the same executor via the kernel. If all
        // executor threads are busy, the reentrant call will queue up behind the blocking
        // set_stream_size() call and never complete.
        unblock(move || vmo.set_stream_size(new_size)).await?;

        let previous_content_size = self.handle.get_size();
        let mut inner = self.inner.lock().unwrap();
        if new_size < previous_content_size {
            inner.pending_shrink = match inner.pending_shrink {
                PendingShrink::None => PendingShrink::ShrinkTo(new_size),
                PendingShrink::ShrinkTo(size) => {
                    PendingShrink::ShrinkTo(std::cmp::min(size, new_size))
                }
                PendingShrink::NeedsTrim => PendingShrink::ShrinkTo(new_size),
            }
        }

        // Not all paths through the resize method above cause the modification time in the kernel
        // to be set (e.g. if only the stream size is changed), so force an mtime update here.
        let _ = self.was_file_modified_since_last_call()?;
        inner.dirty_mtime = DirtyTimestamp::Some(Timestamp::now());

        // There may be reservations for dirty pages that are no longer relevant but the locations
        // of the pages is not tracked so they are assumed to still be dirty. This will get
        // rectified on the next flush.
        Ok(())
    }

    pub async fn update_attributes(
        &self,
        attributes: &fio::MutableNodeAttributes,
    ) -> Result<(), Error> {
        let empty_attributes = fio::MutableNodeAttributes { ..Default::default() };
        if *attributes == empty_attributes {
            return Ok(());
        }

        // A race condition can occur if another flush occurs between now and the end of the
        // transaction. This lock is to prevent another flush from occurring during that time.
        let fs;
        // The _flush_guard persists until the end of the function
        let _flush_guard;
        let set_creation_time = attributes.creation_time.is_some();
        let set_modification_time = attributes.modification_time.is_some();
        let (attributes_with_pending_mtime, ctime) = {
            let store = self.handle.store();
            fs = store.filesystem();
            let keys =
                lock_keys![LockKey::truncate(store.store_object_id(), self.handle.object_id())];
            _flush_guard = fs.lock_manager().write_lock(keys).await;
            let mut inner = self.inner.lock().unwrap();
            let mut attributes = attributes.clone();
            // There is an assumption that when we expose ctime and mtime, that ctime is the same
            // as dirty_mtime (when it is some value). When we call `update_attributes(..)`,
            // a situation could arise where ctime is ahead of dirty_mtime and that assumption is no
            // longer true. An example of this is when we call `update_attributes(..)` without
            // setting mtime. In this case, we can no longer assume ctime is equal to dirty_mtime.
            // A way around this is to update attributes with dirty_mtime whenever mtime is not
            // passed in explicitly which will reset dirty_mtime upon successful completion.
            let dirty_mtime = inner
                .dirty_mtime
                .begin_flush(self.was_file_modified_since_last_call()?)
                .map(|t| t.as_nanos());
            if !set_modification_time {
                attributes.modification_time = dirty_mtime;
            }
            (attributes, Some(Timestamp::now()))
        };

        let mut transaction = self.handle.new_transaction().await?;
        self.handle
            .update_attributes(&mut transaction, Some(&attributes_with_pending_mtime), ctime)
            .await
            .context("update_attributes failed")?;
        transaction.commit().await.context("Failed to commit transaction")?;
        // Any changes to the creation_time before this transaction are superseded by the values
        // set in this update.
        {
            let mut inner = self.inner.lock().unwrap();
            if set_creation_time {
                inner.dirty_crtime = DirtyTimestamp::None;
            }
            // Discard changes to dirty_mtime if no further update was made since begin_flush(..).
            inner.dirty_mtime.end_flush();
        }

        Ok(())
    }

    pub async fn get_properties(&self) -> Result<ObjectProperties, Error> {
        // We must extract information from `inner` *before* we try and retrieve the properties from
        // the handle to avoid a window where we might see old properties.  When we flush, we update
        // the handle and *then* remove the properties from `inner`.
        let (dirty_page_count, data_size, crtime, mtime) = {
            let mut inner = self.inner.lock().unwrap();

            // If there are no dirty pages, the client can't have modified anything.
            if inner.dirty_page_count > 0 && self.was_file_modified_since_last_call()? {
                inner.dirty_mtime = DirtyTimestamp::Some(Timestamp::now());
            }
            (
                inner.dirty_page_count,
                self.vmo.get_stream_size()?,
                inner.dirty_crtime.timestamp(),
                inner.dirty_mtime.timestamp(),
            )
        };
        let mut props = self.handle.get_properties().await?;
        props.allocated_size += dirty_page_count * zx::system_get_page_size() as u64;
        props.data_attribute_size = data_size;
        if let Some(t) = crtime {
            props.creation_time = t;
        }
        if let Some(t) = mtime {
            props.modification_time = t;
            props.change_time = t;
        }
        Ok(props)
    }

    /// Returns true if the handle needs flushing.
    pub fn needs_flush(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.dirty_crtime.needs_flush()
            || inner.dirty_mtime.needs_flush()
            || inner.dirty_page_count > 0
            || inner.pending_shrink != PendingShrink::None
        {
            return true;
        }
        match self.was_file_modified_since_last_call() {
            Ok(true) => {
                inner.dirty_mtime = DirtyTimestamp::Some(Timestamp::now());
                true
            }
            Ok(false) => false,
            Err(_) => {
                // We can't return errors, so play it safe and assume the file needs flushing.
                true
            }
        }
    }

    /// Pre-allocate a region of this file on-disk.
    #[cfg(test)]
    pub async fn allocate(&self, range: Range<u64>) -> Result<(), Error> {
        // Flushing relies on the allocated ranges to determine flush batching, so we grab the same
        // lock as flushing to make sure they are mutually exclusive.
        let store = self.store();
        let fs = store.filesystem();
        let keys = lock_keys![LockKey::truncate(store.store_object_id(), self.handle.object_id())];
        let _flush_guard = fs.lock_manager().write_lock(keys).await;
        // Allocate extends the file if the range is beyond the current file size, so update the
        // stream size in that case as well.
        if self.handle.get_size() < range.end {
            let vmo = self.vmo.temp_clone();
            // Similar to truncate above, this unblock is to break an executor ordering deadlock
            // situation. Vmo::set_stream_size() may trigger a blocking call back into Fxfs on the
            // same executor via the kernel. If all executor threads are busy, the reentrant call
            // will queue up behind the blocking set_stream_size() call and never complete.
            unblock(move || vmo.set_stream_size(range.end)).await?;
        }
        self.handle.allocate(range).await
    }
}

impl Drop for PagedObjectHandle {
    fn drop(&mut self) {
        let inner = self.inner.lock().unwrap();
        let reservation = inner.reservation();
        if reservation > 0 {
            self.allocator().release_reservation(Some(self.store().store_object_id()), reservation);
        }
    }
}

impl ObjectHandle for PagedObjectHandle {
    fn set_trace(&self, v: bool) {
        self.handle.set_trace(v);
    }
    fn object_id(&self) -> u64 {
        self.handle.object_id()
    }
    fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
        self.handle.allocate_buffer(size)
    }
    fn block_size(&self) -> u64 {
        self.handle.block_size()
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum BatchMode {
    Zero,
    Cow,
    Overwrite,
}

/// Manages the batching of pages to flush per transaction, making sure that each batch stays below
/// FLUSH_BATCH_SIZE, the number of bytes that should be flushed in a single transaction. This
/// prevents transactions from growing larger than can be handled at once.
///
/// This also splits the batches between ranges which should be written using CoW semantics and
/// ranges which should be written using Overwrite semantics, because the transaction options are
/// different.
#[derive(Default, Debug)]
struct FlushBatches {
    batches: Vec<FlushBatch>,
    working_cow_batch: Option<FlushBatch>,
    working_overwrite_batch: Option<FlushBatch>,

    /// The number of new pages to be written spanned by the `batches`, which will use the running
    /// reservation. This does not include zero ranges or writes to ranges that are already
    /// allocated.
    dirty_reserved_count: u64,

    /// The number of pages that were marked dirty but are not included in `batches` because they
    /// don't need to be flushed. These are pages that were beyond the VMO's stream size.
    skipped_dirty_page_count: u64,

    /// Any zero ranges get put into their own batch. Zero ranges don't actually add any metadata
    /// at the moment (and will error if they do) so we don't need to split them up.
    zero_batch: Option<FlushBatch>,
}

impl FlushBatches {
    fn add_range(&mut self, range: Range<u64>, mode: BatchMode) {
        let working_batch_ref = match mode {
            BatchMode::Zero => {
                self.zero_batch.get_or_insert_with(|| FlushBatch::new(mode)).add_range(range);
                return;
            }
            BatchMode::Cow => &mut self.working_cow_batch,
            BatchMode::Overwrite => &mut self.working_overwrite_batch,
        };
        let mut working_batch = working_batch_ref.get_or_insert_with(|| FlushBatch::new(mode));
        if mode == BatchMode::Cow {
            self.dirty_reserved_count += page_count(range.clone());
        }
        let mut remaining = working_batch.add_range(range);
        while let Some(range) = remaining {
            self.batches.push(working_batch_ref.take().unwrap());
            working_batch = working_batch_ref.get_or_insert_with(|| FlushBatch::new(mode));
            remaining = working_batch.add_range(range);
        }
    }

    fn skip_range(&mut self, range: Range<u64>) {
        self.skipped_dirty_page_count += page_count(range);
    }

    fn consume(mut self) -> (Vec<FlushBatch>, u64, u64) {
        if let Some(batch) = self.working_cow_batch {
            self.batches.push(batch);
        }
        if let Some(batch) = self.working_overwrite_batch {
            self.batches.push(batch);
        }
        if let Some(batch) = self.zero_batch {
            self.batches.push(batch)
        }
        (self.batches, self.dirty_reserved_count, self.skipped_dirty_page_count)
    }
}

#[derive(Debug, PartialEq)]
struct FlushBatch {
    /// The ranges to be flushed in this batch.
    ranges: Vec<Range<u64>>,

    /// The number of bytes spanned by `ranges`, excluding zero ranges if this is a CoW batch.
    dirty_byte_count: u64,

    /// The mode of this batch.
    mode: BatchMode,
}

impl FlushBatch {
    fn new(mode: BatchMode) -> Self {
        Self { ranges: Vec::new(), dirty_byte_count: 0, mode }
    }

    /// Adds `range` to this batch. If `range` doesn't entirely fit into this batch then the
    /// remaining part of the range is returned.
    fn add_range(&mut self, range: Range<u64>) -> Option<Range<u64>> {
        debug_assert!(range.start >= self.ranges.last().map_or(0, |r| r.end));
        if self.mode == BatchMode::Zero {
            self.ranges.push(range);
            return None;
        }

        let split_point = range.start + (FLUSH_BATCH_SIZE - self.dirty_byte_count);
        let (range, remaining) = range.split(split_point);

        if let Some(range) = range {
            self.dirty_byte_count += range.end - range.start;
            self.ranges.push(range);
        }

        remaining
    }

    fn page_count(&self) -> u64 {
        how_many(self.dirty_byte_count, zx::system_get_page_size())
    }

    fn writeback_begin(&self, vmo: &zx::Vmo, pager: &Pager) {
        let options = match self.mode {
            BatchMode::Zero => zx::PagerWritebackBeginOptions::DIRTY_RANGE_IS_ZERO,
            BatchMode::Cow | BatchMode::Overwrite => zx::PagerWritebackBeginOptions::empty(),
        };
        for range in &self.ranges {
            pager.writeback_begin(vmo, range.clone(), options);
        }
    }

    fn writeback_end(&self, vmo: &zx::Vmo, pager: &Pager) {
        for range in &self.ranges {
            pager.writeback_end(vmo, range.clone());
        }
    }

    async fn add_to_transaction<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        vmo: &zx::Vmo,
        handle: &'a DataObjectHandle<FxVolume>,
        content_size: u64,
    ) -> Result<(), Error> {
        if self.mode == BatchMode::Zero {
            for range in &self.ranges {
                // TODO(https://fxbug.dev/349447236): This doesn't seem to ever do anything, so
                // this experimental assert is going to sit around for a bit to see if there is a
                // case we aren't aware of.
                let pre_zero_len = transaction.mutations().len();
                handle.zero(transaction, range.clone()).await.context("zeroing a range failed")?;
                assert_eq!(pre_zero_len, transaction.mutations().len());
            }
            return Ok(());
        }

        if self.dirty_byte_count > 0 {
            let mut buffer =
                handle.allocate_buffer(self.dirty_byte_count.try_into().unwrap()).await;
            let mut slice = buffer.as_mut_slice();

            let mut dirty_ranges = Vec::new();
            for range in &self.ranges {
                let range = range.clone();
                let (head, tail) = slice.split_at_mut(
                    (std::cmp::min(range.end, content_size) - range.start).try_into().unwrap(),
                );
                vmo.read(head, range.start)?;
                slice = tail;
                // Zero out the tail.
                if range.end > content_size {
                    let (head, tail) = slice.split_at_mut((range.end - content_size) as usize);
                    head.fill(0);
                    slice = tail;
                }
                dirty_ranges.push(range);
            }
            match self.mode {
                BatchMode::Zero => unreachable!(),
                BatchMode::Cow => handle
                    .multi_write(transaction, 0, &dirty_ranges, buffer.as_mut())
                    .await
                    .context("multi_write failed")?,
                BatchMode::Overwrite => handle
                    .multi_overwrite(transaction, 0, &dirty_ranges, buffer.as_mut())
                    .await
                    .context("multi_overwrite failed")?,
            }
        }

        Ok(())
    }

    fn end(&self) -> u64 {
        self.ranges.last().map(|r| r.end).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuchsia::directory::FxDirectory;
    use crate::fuchsia::node::FxNode;
    use crate::fuchsia::pager::{default_page_in, PageInRange, PagerPacketReceiverRegistration};
    use crate::fuchsia::testing::{
        close_dir_checked, close_file_checked, open_file_checked, TestFixture,
    };
    use crate::fuchsia::volume::FxVolumeAndRoot;
    use anyhow::bail;
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_proxy, ServerEnd};
    use fuchsia_fs::file;
    use futures::channel::mpsc::{unbounded, UnboundedSender};
    use futures::{join, StreamExt};
    use fxfs::filesystem::{FxFilesystemBuilder, OpenFxFilesystem};
    use fxfs::object_store::volume::root_volume;
    use fxfs::object_store::Directory;
    use fxfs_macros::ToWeakNode;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
    use std::sync::{Condvar, Weak};
    use std::time::Duration;
    use storage_device::fake_device::FakeDevice;
    use storage_device::{buffer, DeviceHolder};
    use test_util::{assert_geq, assert_lt};
    use vfs::path::Path;
    use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

    const BLOCK_SIZE: u32 = 512;
    const BLOCK_COUNT: u64 = 16384;
    const FILE_NAME: &str = "file";
    const ONE_DAY: u64 = Duration::from_secs(60 * 60 * 24).as_nanos() as u64;

    async fn get_attrs_checked(file: &fio::FileProxy) -> fio::NodeAttributes {
        let (status, attrs) = file.get_attr().await.expect("FIDL call failed");
        zx::Status::ok(status).expect("get_attr failed");
        attrs
    }

    async fn get_attributes_checked(
        file: &fio::FileProxy,
        query: fio::NodeAttributesQuery,
    ) -> fio::NodeAttributes2 {
        let (mutable_attributes, immutable_attributes) = file
            .get_attributes(query)
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("get_attributes failed");
        fio::NodeAttributes2 { mutable_attributes, immutable_attributes }
    }

    async fn get_attrs_and_attributes_parity_checked(file: &fio::FileProxy) {
        let attrs = get_attrs_checked(&file).await;
        let attributes = get_attributes_checked(
            &file,
            fio::NodeAttributesQuery::ID
                | fio::NodeAttributesQuery::CONTENT_SIZE
                | fio::NodeAttributesQuery::STORAGE_SIZE
                | fio::NodeAttributesQuery::LINK_COUNT
                | fio::NodeAttributesQuery::CREATION_TIME
                | fio::NodeAttributesQuery::MODIFICATION_TIME,
        )
        .await;
        assert_eq!(attrs.id, attributes.immutable_attributes.id.expect("get_attributes failed"));
        assert_eq!(
            attrs.content_size,
            attributes.immutable_attributes.content_size.expect("get_attributes failed")
        );
        assert_eq!(
            attrs.storage_size,
            attributes.immutable_attributes.storage_size.expect("get_attributes failed")
        );
        assert_eq!(
            attrs.link_count,
            attributes.immutable_attributes.link_count.expect("get_attributes failed")
        );
        assert_eq!(
            attrs.creation_time,
            attributes.mutable_attributes.creation_time.expect("get_attributes failed")
        );
        assert_eq!(
            attrs.modification_time,
            attributes.mutable_attributes.modification_time.expect("get_attributes failed")
        );
    }

    async fn update_attributes_checked(
        file: &fio::FileProxy,
        attributes: &fio::MutableNodeAttributes,
    ) {
        file.update_attributes(&attributes)
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("update_attributes failed");
    }

    async fn open_filesystem(
        pre_commit_hook: impl Fn(&Transaction<'_>) -> Result<(), Error> + Send + Sync + 'static,
    ) -> (OpenFxFilesystem, FxVolumeAndRoot) {
        let device = DeviceHolder::new(FakeDevice::new(BLOCK_COUNT, BLOCK_SIZE));
        let fs = FxFilesystemBuilder::new()
            .pre_commit_hook(pre_commit_hook)
            .format(true)
            .open(device)
            .await
            .unwrap();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", None).await.unwrap();
        let store_object_id = store.store_object_id();
        let volume =
            FxVolumeAndRoot::new::<FxDirectory>(Weak::new(), store, store_object_id).await.unwrap();
        (fs, volume)
    }

    fn open_volume(volume: &FxVolumeAndRoot) -> fio::DirectoryProxy {
        let (root, server_end) =
            create_proxy::<fio::DirectoryMarker>().expect("create_proxy failed");
        volume.root().clone().as_directory().open(
            volume.volume().scope().clone(),
            fio::OpenFlags::DIRECTORY
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE,
            Path::dot(),
            ServerEnd::new(server_end.into_channel()),
        );
        root
    }

    #[fuchsia::test]
    async fn test_large_flush_requiring_multiple_transactions() {
        let transaction_count = Arc::new(AtomicU64::new(0));
        let (fs, volume) = open_filesystem({
            let transaction_count = transaction_count.clone();
            move |_| {
                transaction_count.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        })
        .await;
        let root = open_volume(&volume);

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;
        let info = file.describe().await.unwrap();
        let stream: zx::Stream = info.stream.unwrap();

        // Touch enough pages that 3 transaction will be required.
        unblock(move || {
            let page_size = zx::system_get_page_size() as u64;
            let write_count: u64 = (FLUSH_BATCH_SIZE / page_size) * 2 + 10;
            for i in 0..write_count {
                stream
                    .write_at(zx::StreamWriteOptions::empty(), i * page_size, &[0, 1, 2, 3, 4])
                    .expect("write should succeed");
            }
        })
        .await;

        transaction_count.store(0, Ordering::Relaxed);
        file.sync().await.unwrap().unwrap();
        assert_eq!(transaction_count.load(Ordering::Relaxed), 3);

        close_file_checked(file).await;
        close_dir_checked(root).await;
        volume.volume().terminate().await;
        fs.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test]
    async fn test_multi_transaction_flush_with_failing_middle_transaction() {
        let fail_transaction_after = Arc::new(AtomicI64::new(i64::MAX));
        let (fs, volume) = open_filesystem({
            let fail_transaction_after = fail_transaction_after.clone();
            move |_| {
                if fail_transaction_after.fetch_sub(1, Ordering::Relaxed) < 1 {
                    bail!("Intentionally fail transaction")
                } else {
                    Ok(())
                }
            }
        })
        .await;
        let root = open_volume(&volume);

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        let info = file.describe().await.unwrap();
        let stream: zx::Stream = info.stream.unwrap();
        // Touch enough pages that 3 transaction will be required.
        unblock(move || {
            let page_size = zx::system_get_page_size() as u64;
            let write_count: u64 = (FLUSH_BATCH_SIZE / page_size) * 2 + 10;
            for i in 0..write_count {
                stream
                    .write_at(zx::StreamWriteOptions::empty(), i * page_size, &i.to_le_bytes())
                    .expect("write should succeed");
            }
        })
        .await;

        // Succeed the multi_write call from the first transaction and fail the multi_write call
        // from the second transaction. The metadata from all of the transactions doesn't get
        // written to disk until the journal is synced which happens in FxFile::sync after all of
        // the multi_writes.
        fail_transaction_after.store(1, Ordering::Relaxed);
        file.sync().await.unwrap().expect_err("sync should fail");
        fail_transaction_after.store(i64::MAX, Ordering::Relaxed);

        // This sync will panic if the allocator reservations intended for the second or third
        // transactions weren't retained or the pages in the first transaction weren't properly
        // cleaned.
        file.sync().await.unwrap().expect("sync should succeed");

        close_file_checked(file).await;
        close_dir_checked(root).await;
        volume.volume().terminate().await;
        fs.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test]
    async fn test_writeback_begin_and_end_are_called_correctly() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;
        let info = file.describe().await.expect("describe failed");
        let stream = Arc::new(info.stream.unwrap());

        let page_size = zx::system_get_page_size() as u64;
        let write_count: u64 = (FLUSH_BATCH_SIZE / page_size) * 2 + 10;

        {
            let stream = stream.clone();
            unblock(move || {
                // Dirty lots of pages so multiple transactions are required.
                for i in 0..(write_count * 2) {
                    stream
                        .write_at(zx::StreamWriteOptions::empty(), i * page_size, &[0, 1, 2, 3, 4])
                        .unwrap();
                }
            })
            .await;
        }
        // Sync the file to mark all of pages as clean.
        file.sync().await.unwrap().unwrap();
        // Set the file size to 0 to mark all of the cleaned pages as zero pages.
        file.resize(0).await.unwrap().unwrap();

        {
            let stream = stream.clone();
            unblock(move || {
                // Write to every other page to force alternating zero and dirty pages.
                for i in 0..write_count {
                    stream
                        .write_at(
                            zx::StreamWriteOptions::empty(),
                            i * page_size * 2,
                            &[0, 1, 2, 3, 4],
                        )
                        .unwrap();
                }
            })
            .await;
        }
        // Sync to mark everything as clean again.
        file.sync().await.unwrap().unwrap();

        // Touch a single page so another flush is required.
        unblock(move || {
            stream.write_at(zx::StreamWriteOptions::empty(), 0, &[0, 1, 2, 3, 4]).unwrap()
        })
        .await;

        // If writeback_begin and writeback_end weren't called in the correct order in the previous
        // sync then not all of the pages will have been marked clean. If not all of the pages were
        // cleaned then this sync will panic because there won't be enough reserved space to clean
        // the pages that weren't properly cleaned in the previous sync.
        file.sync().await.unwrap().unwrap();

        close_file_checked(file).await;
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_writing_overrides_set_mtime() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        let initial_time = get_attrs_checked(&file).await.modification_time;
        // Advance the mtime by a large amount that should be reachable by the test.
        update_attributes_checked(
            &file,
            &fio::MutableNodeAttributes {
                modification_time: Some(initial_time + ONE_DAY),
                ..Default::default()
            },
        )
        .await;

        let updated_time = get_attrs_checked(&file).await.modification_time;
        assert!(updated_time > initial_time);

        file::write(&file, &[1, 2, 3, 4]).await.expect("write failed");

        // Writing to the file after advancing the mtime will bring the mtime back to the current
        // time.
        let current_mtime = get_attrs_checked(&file).await.modification_time;
        assert!(current_mtime < updated_time);

        file.sync().await.unwrap().unwrap();
        let synced_mtime = get_attrs_checked(&file).await.modification_time;
        assert_eq!(synced_mtime, current_mtime);

        close_file_checked(file).await;
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_flushing_after_get_attr_does_not_change_mtime() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        file.write(&[1, 2, 3, 4])
            .await
            .expect("FIDL call failed")
            .map_err(zx::Status::from_raw)
            .expect("write failed");

        let first_mtime = get_attrs_checked(&file).await.modification_time;

        // The contents of the file haven't changed since get_attr was called so the flushed mtime
        // should be the same as the mtime returned from the get_attr call.
        file.sync().await.unwrap().unwrap();
        let flushed_mtime = get_attrs_checked(&file).await.modification_time;
        assert_eq!(flushed_mtime, first_mtime);

        close_file_checked(file).await;
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_timestamps_are_preserved_across_flush_failures() {
        let fail_transaction = Arc::new(AtomicBool::new(false));
        let (fs, volume) = open_filesystem({
            let fail_transaction = fail_transaction.clone();
            move |_| {
                if fail_transaction.load(Ordering::Relaxed) {
                    Err(zx::Status::IO).context("Intentionally fail transaction")
                } else {
                    Ok(())
                }
            }
        })
        .await;
        let root = open_volume(&volume);

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;
        file::write(&file, [1, 2, 3, 4]).await.unwrap();

        let attrs = get_attrs_checked(&file).await;
        let future = attrs.creation_time + ONE_DAY;
        update_attributes_checked(
            &file,
            &fio::MutableNodeAttributes {
                creation_time: Some(future),
                modification_time: Some(future),
                ..Default::default()
            },
        )
        .await;

        fail_transaction.store(true, Ordering::Relaxed);
        file.sync().await.unwrap().expect_err("sync should fail");
        fail_transaction.store(false, Ordering::Relaxed);

        let attrs = get_attrs_checked(&file).await;
        assert_eq!(attrs.creation_time, future);
        assert_eq!(attrs.modification_time, future);

        close_file_checked(file).await;
        close_dir_checked(root).await;
        volume.volume().terminate().await;
        fs.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test]
    async fn test_max_file_size() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;
        let info = file.describe().await.unwrap();
        let stream: zx::Stream = info.stream.unwrap();

        unblock(move || {
            stream
                .write_at(zx::StreamWriteOptions::empty(), MAX_FILE_SIZE - 1, &[1])
                .expect("write should succeed");
            stream
                .write_at(zx::StreamWriteOptions::empty(), MAX_FILE_SIZE, &[1])
                .expect_err("write should fail");
        })
        .await;
        assert_eq!(get_attrs_checked(&file).await.content_size, MAX_FILE_SIZE);

        file.resize(MAX_FILE_SIZE).await.unwrap().expect("resize should succeed");
        file.resize(MAX_FILE_SIZE + 1).await.unwrap().expect_err("resize should fail");

        close_file_checked(file).await;
        fixture.close().await;
    }

    #[test]
    fn test_reservation_needed() {
        let page_size = zx::system_get_page_size() as u64;
        assert_eq!(FLUSH_BATCH_SIZE / page_size, 128);

        assert_eq!(reservation_needed(0), 0);

        assert_eq!(reservation_needed(1), TRANSACTION_METADATA_MAX_AMOUNT + 1 * page_size);
        assert_eq!(reservation_needed(10), TRANSACTION_METADATA_MAX_AMOUNT + 10 * page_size);
        assert_eq!(reservation_needed(128), TRANSACTION_METADATA_MAX_AMOUNT + 128 * page_size);

        assert_eq!(reservation_needed(129), 2 * TRANSACTION_METADATA_MAX_AMOUNT + 129 * page_size);
        assert_eq!(reservation_needed(256), 2 * TRANSACTION_METADATA_MAX_AMOUNT + 256 * page_size);

        assert_eq!(
            reservation_needed(1500),
            12 * TRANSACTION_METADATA_MAX_AMOUNT + 1500 * page_size
        );
    }

    #[test]
    fn test_flush_batch_page_count() {
        let mut flush_batch = FlushBatch::new(BatchMode::Cow);
        assert_eq!(flush_batch.page_count(), 0);

        flush_batch.add_range(4096..8192);
        assert_eq!(flush_batch.page_count(), 1);

        // Adding a partial page rounds up to the next page. Only the page containing the content
        // size should be a partial page so handling multiple partial pages isn't necessary.
        flush_batch.add_range(8192..8704);
        assert_eq!(flush_batch.page_count(), 2);
    }

    #[test]
    fn test_flush_batch_add_range_splits_range() {
        let mut flush_batch = FlushBatch::new(BatchMode::Cow);

        let remaining = flush_batch.add_range(0..(FLUSH_BATCH_SIZE + 4096));
        let remaining = remaining.expect("The batch should have run out of space");
        assert_eq!(remaining, FLUSH_BATCH_SIZE..(FLUSH_BATCH_SIZE + 4096));

        let range = (FLUSH_BATCH_SIZE + 4096)..(FLUSH_BATCH_SIZE + 8192);
        assert_eq!(flush_batch.add_range(range.clone()), Some(range));
    }

    #[test]
    fn test_flush_batches_add_range_huge_range() {
        let mut batches = FlushBatches::default();
        batches.add_range(0..(FLUSH_BATCH_SIZE * 2 + 8192), BatchMode::Cow);
        let (batches, dirty_reserved_count, skipped_dirty_page_count) = batches.consume();
        assert_eq!(dirty_reserved_count, 258);
        assert_eq!(skipped_dirty_page_count, 0);
        assert_eq!(
            batches,
            vec![
                FlushBatch {
                    ranges: vec![0..FLUSH_BATCH_SIZE],
                    dirty_byte_count: FLUSH_BATCH_SIZE,
                    mode: BatchMode::Cow,
                },
                FlushBatch {
                    ranges: vec![FLUSH_BATCH_SIZE..(FLUSH_BATCH_SIZE * 2)],
                    dirty_byte_count: FLUSH_BATCH_SIZE,
                    mode: BatchMode::Cow,
                },
                FlushBatch {
                    ranges: vec![(FLUSH_BATCH_SIZE * 2)..(FLUSH_BATCH_SIZE * 2 + 8192)],
                    dirty_byte_count: 8192,
                    mode: BatchMode::Cow,
                }
            ]
        );
    }

    #[test]
    fn test_flush_batches_add_range_multiple_ranges() {
        let page_size = zx::system_get_page_size() as u64;
        let mut batches = FlushBatches::default();
        batches.add_range(0..page_size, BatchMode::Cow);
        batches.add_range(page_size..(page_size * 3), BatchMode::Zero);
        batches.add_range((page_size * 7)..(page_size * 150), BatchMode::Cow);
        batches.add_range((page_size * 200)..(page_size * 500), BatchMode::Zero);
        batches.add_range((page_size * 500)..(page_size * 650), BatchMode::Overwrite);

        let (batches, dirty_reserved_count, skipped_dirty_page_count) = batches.consume();
        assert_eq!(dirty_reserved_count, 144);
        assert_eq!(skipped_dirty_page_count, 0);
        assert_eq!(
            batches,
            vec![
                FlushBatch {
                    ranges: vec![0..page_size, (page_size * 7)..(page_size * 134)],
                    dirty_byte_count: FLUSH_BATCH_SIZE,
                    mode: BatchMode::Cow,
                },
                FlushBatch {
                    ranges: vec![(page_size * 500)..(page_size * 628)],
                    dirty_byte_count: FLUSH_BATCH_SIZE,
                    mode: BatchMode::Overwrite,
                },
                FlushBatch {
                    ranges: vec![(page_size * 134)..(page_size * 150),],
                    dirty_byte_count: 16 * page_size,
                    mode: BatchMode::Cow,
                },
                FlushBatch {
                    ranges: vec![(page_size * 628)..(page_size * 650),],
                    dirty_byte_count: 22 * page_size,
                    mode: BatchMode::Overwrite,
                },
                FlushBatch {
                    ranges: vec![page_size..(page_size * 3), (page_size * 200)..(page_size * 500)],
                    dirty_byte_count: 0,
                    mode: BatchMode::Zero,
                },
            ]
        );
    }

    #[test]
    fn test_flush_batches_skip_range() {
        let mut batches = FlushBatches::default();
        batches.skip_range(0..8192);
        let (batches, dirty_reserved_count, skipped_dirty_page_count) = batches.consume();
        assert_eq!(dirty_reserved_count, 0);
        assert_eq!(batches, Vec::new());
        assert_eq!(skipped_dirty_page_count, 2);
    }

    #[fuchsia::test]
    async fn test_retry_shrink_transaction() {
        let fail_transaction = Arc::new(AtomicBool::new(false));
        let (fs, volume) = open_filesystem({
            let fail_transaction = fail_transaction.clone();
            move |_| {
                if fail_transaction.load(Ordering::Relaxed) {
                    bail!("Intentionally fail transaction")
                } else {
                    Ok(())
                }
            }
        })
        .await;
        let root = open_volume(&volume);

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;
        let initial_file_size = zx::system_get_page_size() as usize * 10;
        file::write(&file, vec![5u8; initial_file_size]).await.unwrap();
        file.sync().await.unwrap().map_err(zx::ok).unwrap();
        let initial_attrs = get_attrs_checked(&file).await;
        assert_geq!(initial_attrs.storage_size, initial_file_size as u64);
        file.resize(0).await.unwrap().map_err(zx::ok).unwrap();

        fail_transaction.store(true, Ordering::Relaxed);
        file.sync().await.unwrap().expect_err("flush should have failed");
        fail_transaction.store(false, Ordering::Relaxed);

        // Verify that the file wasn't resized and non of the blocks were freed.
        let attrs = get_attrs_checked(&file).await;
        assert_eq!(attrs.storage_size, initial_attrs.storage_size);

        file.sync().await.unwrap().map_err(zx::ok).unwrap();
        let attrs = get_attrs_checked(&file).await;
        // The shrink transaction was retried and the blocks were freed.
        assert_eq!(attrs.storage_size, 0);

        close_file_checked(file).await;
        close_dir_checked(root).await;
        volume.volume().terminate().await;
        fs.close().await.expect("close filesystem failed");
    }

    #[fuchsia::test]
    async fn test_retry_trim_transaction() {
        let fail_transaction_after = Arc::new(AtomicI64::new(i64::MAX));
        let (fs, volume) = open_filesystem({
            let fail_transaction_after = fail_transaction_after.clone();
            move |_| {
                if fail_transaction_after.fetch_sub(1, Ordering::Relaxed) < 1 {
                    bail!("Intentionally fail transaction")
                } else {
                    Ok(())
                }
            }
        })
        .await;
        let root = open_volume(&volume);

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;
        let page_size = zx::system_get_page_size() as u64;
        // Write to every other page to generate lots of small extents that will require multiple
        // transactions to be freed.
        let write_count: u64 = 256;
        for i in 0..write_count {
            file.write_at(&[5u8; 1], page_size * 2 * i)
                .await
                .unwrap()
                .map_err(zx::ok)
                .unwrap_or_else(|e| panic!("Write {} failed {:?}", i, e));
        }
        file.sync().await.unwrap().map_err(zx::ok).unwrap();
        let initial_attrs = get_attrs_checked(&file).await;
        assert_geq!(initial_attrs.storage_size, write_count * page_size);
        file.resize(0).await.unwrap().map_err(zx::ok).unwrap();

        // Allow the shrink transaction, fail the trim transaction.
        fail_transaction_after.store(1, Ordering::Relaxed);
        file.sync().await.unwrap().expect_err("flush should have failed");
        fail_transaction_after.store(i64::MAX, Ordering::Relaxed);

        // Some of the extents will be freed by the shrink transactions but not all of them.
        let attrs = get_attrs_checked(&file).await;
        assert_ne!(attrs.storage_size, 0);
        assert_lt!(attrs.storage_size, initial_attrs.storage_size);

        file.sync().await.unwrap().map_err(zx::ok).unwrap();
        let attrs = get_attrs_checked(&file).await;
        // The trim transaction was retried and the extents were freed.
        assert_eq!(attrs.storage_size, 0);

        close_file_checked(file).await;
        close_dir_checked(root).await;
        volume.volume().terminate().await;
        fs.close().await.expect("close filesystem failed");
    }

    // Growing the file isn't tracked by `truncate` and if it's to a page boundary then the
    // kernel won't mark a page as dirty.
    #[fuchsia::test]
    async fn test_needs_flush_after_growing_file_to_page_boundary() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE | fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;
        let page_size = zx::system_get_page_size() as u64;
        file.resize(page_size).await.unwrap().map_err(zx::ok).unwrap();
        close_file_checked(file).await;

        let file = open_file_checked(
            &root,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;
        let attrs = get_attrs_checked(&file).await;
        assert_eq!(attrs.content_size, page_size);

        close_file_checked(file).await;
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_get_update_attrs_and_attributes_parity() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        // The attributes value returned from `get_attrs` and `get_attributes` (io2) should
        // be equivalent
        get_attrs_and_attributes_parity_checked(&file).await;

        let now = Timestamp::now().as_nanos();
        update_attributes_checked(
            &file,
            &fio::MutableNodeAttributes {
                creation_time: Some(now),
                modification_time: Some(now - ONE_DAY),
                mode: Some(111),
                gid: Some(222),
                ..Default::default()
            },
        )
        .await;
        let updated_attributes = get_attributes_checked(
            &file,
            fio::NodeAttributesQuery::CREATION_TIME
                | fio::NodeAttributesQuery::MODIFICATION_TIME
                | fio::NodeAttributesQuery::MODE
                | fio::NodeAttributesQuery::GID,
        )
        .await;
        let mut expected_attributes = fio::NodeAttributes2 {
            mutable_attributes: fio::MutableNodeAttributes { ..Default::default() },
            immutable_attributes: fio::ImmutableNodeAttributes { ..Default::default() },
        };
        expected_attributes.mutable_attributes.creation_time = Some(now);
        // modification_time should reflect the latest change
        expected_attributes.mutable_attributes.modification_time = Some(now - ONE_DAY);
        expected_attributes.mutable_attributes.mode = Some(111);
        expected_attributes.mutable_attributes.gid = Some(222);
        assert_eq!(updated_attributes, expected_attributes);
        get_attrs_and_attributes_parity_checked(&file).await;

        // Check that updating some of the attributes will not overwrite those that are not updated
        update_attributes_checked(
            &file,
            &fio::MutableNodeAttributes { uid: Some(333), gid: Some(444), ..Default::default() },
        )
        .await;
        let current_attributes = get_attributes_checked(
            &file,
            fio::NodeAttributesQuery::CREATION_TIME
                | fio::NodeAttributesQuery::MODIFICATION_TIME
                | fio::NodeAttributesQuery::MODE
                | fio::NodeAttributesQuery::UID
                | fio::NodeAttributesQuery::GID,
        )
        .await;
        expected_attributes.mutable_attributes.uid = Some(333);
        expected_attributes.mutable_attributes.gid = Some(444);
        assert_eq!(current_attributes, expected_attributes);
        get_attrs_and_attributes_parity_checked(&file).await;

        // The contents of the file hasn't changed, so the flushed attributes should remain the same
        file.sync().await.unwrap().unwrap();
        let synced_attributes = get_attributes_checked(
            &file,
            fio::NodeAttributesQuery::CREATION_TIME
                | fio::NodeAttributesQuery::MODIFICATION_TIME
                | fio::NodeAttributesQuery::MODE
                | fio::NodeAttributesQuery::UID
                | fio::NodeAttributesQuery::GID,
        )
        .await;
        assert_eq!(synced_attributes, expected_attributes);
        get_attrs_and_attributes_parity_checked(&file).await;

        close_file_checked(file).await;
        fixture.close().await;
    }

    // `update_attributes` flushes the attributes. We should check for race conditions where another
    // flush could occur at the same time.
    #[fuchsia::test(threads = 10)]
    async fn test_update_attributes_with_race() {
        let fixture = TestFixture::new_unencrypted().await;
        for i in 1..100 {
            let file_name = format!("file {}", i);
            let file1 = open_file_checked(
                fixture.root(),
                fio::OpenFlags::CREATE
                    | fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::NOT_DIRECTORY,
                &file_name,
            )
            .await;
            let file2 = open_file_checked(
                fixture.root(),
                fio::OpenFlags::RIGHT_READABLE
                    | fio::OpenFlags::RIGHT_WRITABLE
                    | fio::OpenFlags::NOT_DIRECTORY,
                &file_name,
            )
            .await;
            join!(
                fasync::Task::spawn(async move {
                    file1
                        .write("foo".as_bytes())
                        .await
                        .expect("FIDL call failed")
                        .map_err(zx::Status::from_raw)
                        .expect("write failed");
                    let write_modification_time =
                        get_attributes_checked(&file1, fio::NodeAttributesQuery::MODIFICATION_TIME)
                            .await
                            .mutable_attributes
                            .modification_time
                            .expect("get_attributes failed");

                    let now = Timestamp::now().as_nanos();
                    update_attributes_checked(
                        &file1,
                        &fio::MutableNodeAttributes {
                            modification_time: Some(now),
                            mode: Some(111),
                            gid: Some(222),
                            ..Default::default()
                        },
                    )
                    .await;
                    fasync::Timer::new(Duration::from_millis(10)).await;
                    let updated_attributes = get_attributes_checked(
                        &file1,
                        fio::NodeAttributesQuery::MODIFICATION_TIME
                            | fio::NodeAttributesQuery::MODE
                            | fio::NodeAttributesQuery::GID,
                    )
                    .await;

                    assert_ne!(
                        updated_attributes.mutable_attributes.modification_time.unwrap(),
                        write_modification_time
                    );
                    assert_eq!(updated_attributes.mutable_attributes.modification_time, Some(now));
                    assert_eq!(updated_attributes.mutable_attributes.mode, Some(111));
                    assert_eq!(updated_attributes.mutable_attributes.gid, Some(222));
                }),
                fasync::Task::spawn(async move {
                    for _ in 1..50 {
                        // Flush data
                        file2.sync().await.unwrap().unwrap();
                    }
                })
            );
        }
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_write_mtime_ctime() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        file::write(&file, &[1, 2, 3, 4]).await.expect("write failed");
        let write_attributes = get_attributes_checked(&file, fio::NodeAttributesQuery::all()).await;
        assert_eq!(
            write_attributes.mutable_attributes.modification_time,
            write_attributes.immutable_attributes.change_time
        );

        // Do something else that should not change mtime or ctime
        file.seek(fio::SeekOrigin::Start, 0)
            .await
            .expect("FIDL call failed")
            .map_err(zx::ok)
            .expect("seek failed");
        file::read(&file).await.expect("read failed");
        let read_attributes = get_attributes_checked(&file, fio::NodeAttributesQuery::all()).await;
        assert_eq!(
            write_attributes.mutable_attributes.modification_time,
            read_attributes.mutable_attributes.modification_time,
        );
        assert_eq!(
            write_attributes.immutable_attributes.change_time,
            read_attributes.immutable_attributes.change_time,
        );

        // Syncing the file should have no affect on ctime
        file.sync().await.unwrap().unwrap();
        let sync_attributes = get_attributes_checked(&file, fio::NodeAttributesQuery::all()).await;
        assert_eq!(
            write_attributes.mutable_attributes.modification_time,
            sync_attributes.mutable_attributes.modification_time,
        );
        assert_eq!(
            write_attributes.immutable_attributes.change_time,
            sync_attributes.immutable_attributes.change_time,
        );

        close_file_checked(file).await;
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_shrink_and_flush_updates_ctime() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        let initial_file_size = zx::system_get_page_size() as usize * 10;
        file::write(&file, vec![5u8; initial_file_size]).await.unwrap();
        file.sync().await.unwrap().map_err(zx::ok).unwrap();

        let (starting_mtime, starting_ctime) = {
            let attributes = get_attributes_checked(&file, fio::NodeAttributesQuery::all()).await;
            (
                attributes.mutable_attributes.modification_time,
                attributes.immutable_attributes.change_time,
            )
        };

        // Shrink the file size.
        file.resize(0).await.expect("FIDL call failed").expect("resize failed");
        // Check that the change in timestamps are preserved with flush.
        file.sync().await.unwrap().unwrap();

        let (synced_mtime, synced_ctime) = {
            let attributes = get_attributes_checked(&file, fio::NodeAttributesQuery::all()).await;
            (
                attributes.mutable_attributes.modification_time,
                attributes.immutable_attributes.change_time,
            )
        };

        assert!(starting_ctime < synced_ctime);
        assert!(starting_mtime < synced_mtime);
        assert_eq!(synced_ctime, synced_mtime);

        close_file_checked(file).await;
        fixture.close().await;
    }

    #[fuchsia::test(threads = 8)]
    async fn test_race() {
        #[derive(ToWeakNode)]
        struct File {
            notifications: UnboundedSender<Op>,
            handle: PagedObjectHandle,
            unblocked_requests: Mutex<HashSet<u64>>,
            cvar: Condvar,
            pager_packet_receiver_registration: PagerPacketReceiverRegistration<Self>,
        }

        impl File {
            fn unblock(&self, request: u64) {
                self.unblocked_requests.lock().unwrap().insert(request);
                self.cvar.notify_all();
            }
        }

        impl FxNode for File {
            fn object_id(&self) -> u64 {
                self.handle.handle.object_id()
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

        impl PagerBacked for File {
            fn pager(&self) -> &crate::pager::Pager {
                self.handle.owner().pager()
            }

            fn pager_packet_receiver_registration(&self) -> &PagerPacketReceiverRegistration<Self> {
                &self.pager_packet_receiver_registration
            }

            fn vmo(&self) -> &zx::Vmo {
                self.handle.vmo()
            }

            fn page_in(self: Arc<Self>, range: PageInRange<Self>) {
                default_page_in(self, range);
            }

            fn mark_dirty(self: Arc<Self>, range: MarkDirtyRange<Self>) {
                self.handle.mark_dirty(range).unwrap();
            }

            fn on_zero_children(self: Arc<Self>) {}

            fn read_alignment(&self) -> u64 {
                self.handle.block_size()
            }

            fn byte_size(&self) -> u64 {
                self.handle.uncached_size()
            }

            async fn aligned_read(&self, range: Range<u64>) -> Result<buffer::Buffer<'_>, Error> {
                let buffer = self.handle.read_uncached(range).await?;
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
                if let Ok(()) = self.notifications.unbounded_send(Op::AfterAlignedRead(counter)) {
                    let mut unblocked_requests = self.unblocked_requests.lock().unwrap();
                    while !unblocked_requests.remove(&counter) {
                        unblocked_requests = self.cvar.wait(unblocked_requests).unwrap();
                    }
                }
                Ok(buffer)
            }
        }

        #[derive(Debug)]
        enum Op {
            AfterAlignedRead(u64),
        }

        let fixture = TestFixture::new().await;

        let vol = fixture.volume().volume().clone();
        let fs = fixture.fs().clone();

        // Run the test in a separate executor to avoid issues caused by stalling page_in requests
        // (see `page_in` above).
        std::thread::spawn(move || {
            fasync::LocalExecutor::new().run_singlethreaded(async move {
                let root_object_id = vol.store().root_directory_object_id();
                let root_dir = Directory::open(&vol, root_object_id).await.expect("open failed");

                let file;
                let mut transaction = fs
                    .new_transaction(
                        lock_keys![LockKey::object(
                            vol.store().store_object_id(),
                            root_dir.object_id()
                        )],
                        Options::default(),
                    )
                    .await
                    .unwrap();
                file = root_dir
                    .create_child_file(&mut transaction, "foo")
                    .await
                    .expect("create_child_file failed");
                {
                    let mut buf = file.allocate_buffer(100).await;
                    buf.as_mut_slice().fill(1);
                    file.txn_write(&mut transaction, 0, buf.as_ref())
                        .await
                        .expect("txn_write failed");
                }
                transaction.commit().await.unwrap();
                let (notifications, mut receiver) = unbounded();

                let (vmo, pager_packet_receiver_registration) = file
                    .owner()
                    .pager()
                    .create_vmo(
                        file.get_size(),
                        zx::VmoOptions::RESIZABLE | zx::VmoOptions::TRAP_DIRTY,
                    )
                    .unwrap();
                let file = Arc::new(File {
                    notifications,
                    handle: PagedObjectHandle::new(file, vmo),
                    unblocked_requests: Mutex::new(HashSet::new()),
                    cvar: Condvar::new(),
                    pager_packet_receiver_registration,
                });

                file.handle.owner().pager().register_file(&file);

                // Trigger a pager request.
                let cloned_file = file.clone();
                let thread1 = std::thread::spawn(move || {
                    cloned_file.vmo().read_to_vec(0, 10).unwrap();
                });

                // Wait for it.
                let request1 = assert_matches!(
                    receiver.next().await.unwrap(),
                    Op::AfterAlignedRead(request1) => request1
                );

                // Truncate and then grow the file.
                file.handle.truncate(0).await.expect("truncate failed");
                file.handle.truncate(100).await.expect("truncate failed");

                // Unblock the first page request after a delay.  The flush should wait for the
                // request to finish.  If it doesn't, then the page request might finish later and
                // provide the wrong pages.
                let cloned_file = file.clone();
                let thread2 = std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    cloned_file.unblock(request1);
                });

                file.handle.flush().await.expect("flush failed");

                // We don't care what the original VMO read request returned, but reading now should
                // return the new content, i.e. zeroes.  The original page-in request would/will
                // return non-zero content.
                let file_cloned = file.clone();
                let thread3 = std::thread::spawn(move || {
                    assert_eq!(&file_cloned.vmo().read_to_vec(0, 10).unwrap(), &[0; 10]);
                });

                // Wait for the second page request to arrive.
                let request2 = assert_matches!(
                    receiver.next().await.unwrap(),
                    Op::AfterAlignedRead(request2) => request2
                );

                // If the flush didn't wait for the request to finish (it's a bug if it doesn't) we
                // want the first page request to complete before the second one, and the only way
                // we can do that now is to wait.
                fasync::Timer::new(std::time::Duration::from_millis(100)).await;

                // Unblock the second page request.
                file.unblock(request2);

                thread1.join().unwrap();
                thread2.join().unwrap();
                thread3.join().unwrap();
            })
        })
        .join()
        .unwrap();

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_file_vmo_write_beyond_content_size_doesnt_break_flush() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();

        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        // Write out three pages initially.
        let page_size = zx::system_get_page_size() as u64;
        let file_size = page_size * 3;
        fuchsia_fs::file::write(&file, &vec![1u8; file_size as usize]).await.unwrap();
        file.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();

        // Get the backing memory for the file. Confirm the length of the vmo and the reported
        // stream size.
        let vmo = file
            .get_backing_memory(fio::VmoFlags::READ | fio::VmoFlags::WRITE)
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        assert_eq!(vmo.get_stream_size().unwrap(), file_size);

        // Resize the file down to one page. Confirm the stream size is updated, but the vmo size
        // stays the same.
        file.resize(page_size).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        file.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(vmo.get_stream_size().unwrap(), page_size);

        // Write some data to the vmo, beyond the current stream size. This does _not_ update the
        // stream size, but it does make pages dirty beyond the end of the file.
        unblock(move || {
            vmo.write(&[1, 2, 3, 4], page_size * 2).unwrap();
            // Writing this data to the vmo shouldn't update the stream size.
            assert_eq!(vmo.get_stream_size().unwrap(), page_size);
        })
        .await;

        // https://fxbug.dev/318434279 - the dirty pages beyond the end of the file cause shutdown
        // to stall forever, waiting for an infinitely looping flush attempt.
        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_file_allocate() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();
        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        let page_size = zx::system_get_page_size() as u64;
        file::write(&file, &vec![1, 2, 3, 4]).await.unwrap();
        file.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();
        file.allocate(0, page_size, fio::AllocateMode::empty())
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        let data = file.read_at(4, 0).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(data, vec![1, 2, 3, 4]);

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_file_allocate_empty() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();
        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        assert_eq!(
            file.get_attributes(fio::NodeAttributesQuery::CONTENT_SIZE)
                .await
                .unwrap()
                .unwrap()
                .1
                .content_size
                .unwrap(),
            0,
        );

        let page_size = zx::system_get_page_size() as u64;
        file.allocate(0, page_size, fio::AllocateMode::empty())
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        let data = file.read_at(page_size, 0).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(data, vec![0; page_size as usize]);

        assert_eq!(
            file.get_attributes(fio::NodeAttributesQuery::CONTENT_SIZE)
                .await
                .unwrap()
                .unwrap()
                .1
                .content_size
                .unwrap(),
            page_size,
        );

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_file_allocate_write() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();
        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        let page_size = zx::system_get_page_size() as u64;
        file.allocate(0, page_size, fio::AllocateMode::empty())
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        file::write(&file, &vec![1, 2, 3, 4]).await.unwrap();
        file.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();
        let data = file.read_at(4, 0).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(data, vec![1, 2, 3, 4]);

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_file_allocate_write_mixed() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();
        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        let page_size = zx::system_get_page_size() as u64;
        file.allocate(page_size, page_size, fio::AllocateMode::empty())
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        let write_data = (0..20).cycle().take(page_size as usize * 2).collect::<Vec<_>>();
        assert_eq!(
            file.write_at(&write_data, 2048).await.unwrap().map_err(zx::Status::from_raw).unwrap(),
            page_size * 2
        );
        file.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();
        let data =
            file.read_at(page_size * 2, 2048).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(data, write_data);

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_file_allocate_write_disk_full() {
        let fixture = TestFixture::new_unencrypted().await;
        let root = fixture.root();
        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        let page_size = zx::system_get_page_size() as u64;
        file.allocate(0, page_size, fio::AllocateMode::empty())
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        let write_data = (0..20).cycle().take(page_size as usize).collect::<Vec<_>>();
        // Fill up the disk with data.
        loop {
            match file.write(&write_data).await.unwrap().map_err(zx::Status::from_raw) {
                Ok(len) => assert_eq!(len, page_size),
                Err(status) => {
                    assert_eq!(status, zx::Status::NO_SPACE);
                    break;
                }
            }
            file.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();
        }

        // Writing outside the allocated range fails.
        assert_eq!(
            file.write_at(&write_data, page_size).await.unwrap().map_err(zx::Status::from_raw),
            Err(zx::Status::NO_SPACE)
        );

        for _ in 0..100 {
            // Writing inside the allocated range succeeds indefinitely.
            assert_eq!(
                file.write_at(&write_data, 0).await.unwrap().map_err(zx::Status::from_raw).unwrap(),
                page_size
            );
            assert_eq!(
                file.write_at(&write_data, 0).await.unwrap().map_err(zx::Status::from_raw).unwrap(),
                page_size
            );
            file.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();
        }

        // Writing outside the allocated range still fails.
        assert_eq!(
            file.write_at(&write_data, page_size).await.unwrap().map_err(zx::Status::from_raw),
            Err(zx::Status::NO_SPACE)
        );

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_file_allocate_write_restart() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();
        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        let page_size = zx::system_get_page_size() as u64;
        file.allocate(0, page_size * 4, fio::AllocateMode::empty())
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        let write_data = (0..20).cycle().take(page_size as usize).collect::<Vec<_>>();
        let write_data_alternate = (0..15).cycle().take(page_size as usize).collect::<Vec<_>>();
        assert_eq!(
            file.write_at(&write_data, page_size)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            page_size
        );
        assert_eq!(
            file.write_at(&write_data, page_size * 2)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            page_size
        );
        file.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();
        // Sync will make a transaction with whatever we have written. Make sure that there are
        // multiple transactions hitting the same blocks, to try and trip up the replay.
        assert_eq!(
            file.write_at(&write_data_alternate, 0)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            page_size
        );
        assert_eq!(
            file.write_at(&write_data_alternate, page_size)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            page_size
        );
        file.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();

        assert_eq!(
            file.read_at(page_size, 0).await.unwrap().map_err(zx::Status::from_raw).unwrap(),
            write_data_alternate,
        );
        assert_eq!(
            file.read_at(page_size, page_size)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            write_data_alternate,
        );
        assert_eq!(
            file.read_at(page_size, page_size * 2)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            write_data,
        );
        assert_eq!(
            file.read_at(page_size, page_size * 3)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            vec![0; page_size as usize],
        );

        let device = fixture.close().await;

        let fixture = TestFixture::new_with_device(device).await;
        let root = fixture.root();
        let file = open_file_checked(
            &root,
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        assert_eq!(
            file.read_at(page_size, 0).await.unwrap().map_err(zx::Status::from_raw).unwrap(),
            write_data_alternate,
        );
        assert_eq!(
            file.read_at(page_size, page_size)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            write_data_alternate,
        );
        assert_eq!(
            file.read_at(page_size, page_size * 2)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            write_data,
        );
        assert_eq!(
            file.read_at(page_size, page_size * 3)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            vec![0; page_size as usize],
        );

        fixture.close().await;
    }

    #[fuchsia::test]
    async fn test_truncate_allocated_file() {
        let fixture = TestFixture::new().await;
        let root = fixture.root();
        let file = open_file_checked(
            &root,
            fio::OpenFlags::CREATE
                | fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::NOT_DIRECTORY,
            FILE_NAME,
        )
        .await;

        let page_size = zx::system_get_page_size() as u64;
        file.allocate(0, page_size * 2, fio::AllocateMode::empty())
            .await
            .unwrap()
            .map_err(zx::Status::from_raw)
            .unwrap();
        let write_data = (0..20).cycle().take(page_size as usize).collect::<Vec<_>>();
        assert_eq!(
            file.write_at(&write_data, page_size)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            page_size
        );
        file.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();

        file.resize(page_size).await.unwrap().map_err(zx::Status::from_raw).unwrap();
        file.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();

        assert_eq!(
            file.write_at(&write_data, page_size)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            page_size
        );
        file.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();
        assert_eq!(
            file.read_at(page_size, page_size)
                .await
                .unwrap()
                .map_err(zx::Status::from_raw)
                .unwrap(),
            write_data,
        );

        fixture.close().await;
    }
}
