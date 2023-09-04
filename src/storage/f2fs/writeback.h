// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_WRITEBACK_H_
#define SRC_STORAGE_F2FS_WRITEBACK_H_

#include "src/storage/lib/vfs/cpp/journal/background_executor.h"

namespace f2fs {

// F2fs flushes dirty pages when the number of dirty data pages exceeds a half of
// |kMaxDirtyDataPages|.
// TODO: When memorypressure is available with tests, we can remove it.
constexpr int kMaxDirtyDataPages = 51200;

// This class is final because there might be background threads running when its destructor runs
// and that would be unsafe if this class had overridden virtual methods that might get called from
// those background threads.
class Writer final {
 public:
  Writer(Bcache *bc, size_t capacity);
  Writer() = delete;
  Writer(const Writer &) = delete;
  Writer &operator=(const Writer &) = delete;
  Writer(const Writer &&) = delete;
  Writer &operator=(const Writer &&) = delete;
  ~Writer();

  // For writeback tasks. Refer to F2fs::ScheduleWriteback().
  void ScheduleWriteback(fpromise::promise<> task);
  // It moves |pages| to |pages_| and schedules a task to request write I/Os for |pages_|.
  // If |completion| is set, it notifies the caller when the task is completed.
  void ScheduleWriteBlocks(sync_completion_t *completion = nullptr, PageList pages = {},
                           bool flush = true);
  // It schedules a writeback operation for I/Os or VMO_PAGER_OP_WRITEBACK_BEGIN/END.
  void ScheduleTask(fpromise::promise<> task);

 private:
  // It returns a task to be scheduled on |executor_| for write IOs.
  // The task builds StorageOperations for write requests of |pages_| and passes them to
  // RunRequests(). When RunRequests() is completed, it wakes waiters who tried to write the
  // writeback pages that StorageOperations conveys. In addition, it signals |completion| if
  // it is not null.
  fpromise::promise<> GetTaskForWriteIO(sync_completion_t *completion);
  StorageOperations MakeStorageOperations(PageList &to_submit) __TA_EXCLUDES(mutex_);

  std::mutex mutex_;
  PageList pages_ __TA_GUARDED(mutex_);
  std::unique_ptr<StorageBuffer> write_buffer_;
  fs::TransactionHandler *transaction_handler_ = nullptr;
  fpromise::sequencer sequencer_;
  // An executor to run tasks for disk write IOs.
  fs::BackgroundExecutor executor_;
  // An executor to run writeback tasks. Refer to F2fs::ScheduleWriteback().
  fs::BackgroundExecutor writeback_executor_;
};

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_WRITEBACK_H_
