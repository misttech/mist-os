// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_JOURNAL_DATA_STREAMER_H_
#define SRC_STORAGE_LIB_VFS_CPP_JOURNAL_DATA_STREAMER_H_

#include <lib/fpromise/promise.h>
#include <zircon/types.h>

#include <cstddef>
#include <vector>

#include <storage/operation/unbuffered_operation.h>
#include <storage/operation/unbuffered_operations_builder.h>

#include "src/storage/lib/vfs/cpp/journal/journal.h"

namespace fs {

// A class which helps callers write streaming data operations to the underlying device.
//
// This class provides the following value over calling "Journal::WriteData" directly:
//
// Client-side coalescing.
//
//   This class groups together contiguous operations, only transmitting them to the writeback
//   subsystem once necessary.
//
// Avoids blocking for large operations.
//
//   For operations which are larger than the underlying writeback buffer, caution must be taken to
//   avoid blocking on "Journal::WriteData", which creates a reservation, but does not begin
//   execution of the underlying data. To mitigate, this class begins execution of these data writes
//   if they are larger than an internal threshold (which is smaller than the entire writeback
//   buffer).
//
// Allows ordering control over "all data", but not between intermediate data writes.
//
//   Typically, when data operations are issued, the caller cares that they all occur before a
//   following action, but the order between those data operations themselves does not matter. This
//   class enables callers to |Flush()| all prior data requests, without imposing an ordering on the
//   data requests issued by |StreamData()|.
//
// This class is thread-compatible.
class DataStreamer {
 public:
  DataStreamer(fs::Journal* journal, size_t writeback_capacity)
      : journal_(journal), writeback_capacity_(writeback_capacity) {}

  // Issues |operation| to the underlying writeback subsystem (eventually).
  //
  // May cache the operation locally, issuing it to the underlying writeback buffer and executor as
  // necessary. To identify completion, refer to |Flush()|.
  //
  // Multiple invocations of |StreamData()| are not ordered with respect to each other.
  // Promises generated by |StreamData()| are ordered before invocations of |Flush()|.
  void StreamData(storage::UnbufferedOperation operation);

  // Ensures all operations sent to |StreamData| are issued to the executor, and returns a promise
  // representing when all those writes have completed.
  fs::Journal::Promise Flush();

  // Issues locally buffered operations to the executor, and track the resulting promise in
  // |promises_|. The completion of all issued operations can be achieved by invoking |Flush()|.
  void IssueOperations();

 private:
  fs::Journal* journal_;
  const size_t writeback_capacity_;

  // Operations which have been sent to |StreamData|, but which have not been sent to the executor.
  storage::UnbufferedOperationsBuilder operations_;

  // Operations which have been sent to the executor.
  std::vector<fpromise::promise<void, zx_status_t>> promises_;
};

}  // namespace fs

#endif  // SRC_STORAGE_LIB_VFS_CPP_JOURNAL_DATA_STREAMER_H_
