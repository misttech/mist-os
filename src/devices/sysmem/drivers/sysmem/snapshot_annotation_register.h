// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_SNAPSHOT_ANNOTATION_REGISTER_H_
#define SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_SNAPSHOT_ANNOTATION_REGISTER_H_

#include <fidl/fuchsia.feedback/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/sys/cpp/service_directory.h>

namespace sys {
class ServiceDirectory;
}  // namespace sys

/// Sends annotations to Feedback that are attached to its snapshots.
class SnapshotAnnotationRegister {
 public:
  SnapshotAnnotationRegister() = default;
  ~SnapshotAnnotationRegister();

  SnapshotAnnotationRegister(const SnapshotAnnotationRegister& other) = delete;
  SnapshotAnnotationRegister& operator=(const SnapshotAnnotationRegister& other) = delete;

  // Set the ServiceDirectory from which to get fuchsia.feedback.ComponentDataRegister.  This can
  // be nullptr. This can be called again, regardless of whether there was already a previous
  // ServiceDirectory. If not called, or if set to nullptr, no crash annotations are reported.
  // The FIDL client will be bound to the given dispatcher.
  void SetServiceDirectory(std::shared_ptr<sys::ServiceDirectory> service_directory,
                           async_dispatcher_t* dispatcher) __TA_EXCLUDES(lock_);
  void UnsetServiceDirectory() __TA_EXCLUDES(lock_) { SetServiceDirectory(nullptr, nullptr); }

  // Increments the reported number of DMA corruption events detected during the current boot.
  void IncrementNumDmaCorruptions() __TA_EXCLUDES(lock_);

 private:
  // Records what thread it is first called on, and then asserts that all subsequent calls come from
  // the same thread. We use it to ensure that `client_` is only used on the same thread in which it
  // was bound.
  void AssertRunningOnClientThread() __TA_REQUIRES(lock_);

  void Flush() __TA_REQUIRES(lock_);

  std::mutex lock_;
  uint64_t num_dma_corruptions_ __TA_GUARDED(lock_) = 0;
  fidl::Client<fuchsia_feedback::ComponentDataRegister> client_ __TA_GUARDED(lock_);
  std::optional<std::thread::id> client_thread_ __TA_GUARDED(lock_);
};

#endif  // SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_SNAPSHOT_ANNOTATION_REGISTER_H_
