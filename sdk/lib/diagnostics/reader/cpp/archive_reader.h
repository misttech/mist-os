// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DIAGNOSTICS_READER_CPP_ARCHIVE_READER_H_
#define LIB_DIAGNOSTICS_READER_CPP_ARCHIVE_READER_H_

#include <fidl/fuchsia.diagnostics/cpp/fidl.h>
#include <fidl/fuchsia.diagnostics/cpp/markers.h>
#include <lib/async/cpp/executor.h>
#include <lib/diagnostics/reader/cpp/inspect.h>
#include <lib/diagnostics/reader/cpp/logs.h>
#include <lib/fpromise/bridge.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/scope.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/stdcompat/optional.h>

#include <cstdint>
#include <list>
#include <optional>

#include <rapidjson/document.h>

namespace diagnostics::reader {

// Shutdown task that runs on the async loop but not the executor.
// This allows for the executor to be shutdown independently of the
// async loop.
struct ShutdownTask : async_task_t {
  sync_completion_t completion;
};

// ArchiveReader supports reading Inspect data from an Archive.
class ArchiveReader {
 public:
  // Create a new ArchiveReader.
  ArchiveReader(async_dispatcher_t* dispatcher, std::vector<std::string> selectors);

  // Get a snapshot of the Inspect data at the current point in time.
  //
  // Returns an error if the ArchiveAccessorPtr is not bound.
  fpromise::promise<std::vector<InspectData>, std::string> GetInspectSnapshot();

  // Gets a snapshot of the Inspect data at the point in time in which all listed component
  // names are present.
  //
  // Returns an error if the ArchiveAccessorPtr is not bound.
  fpromise::promise<std::vector<InspectData>, std::string> SnapshotInspectUntilPresent(
      std::vector<std::string> component_names);

  // Subscribes to logs using the given `mode`.
  LogsSubscription GetLogs(fuchsia_diagnostics::StreamMode mode);

  ~ArchiveReader();

 private:
  void InnerSnapshotInspectUntilPresent(
      fpromise::completer<std::vector<InspectData>, std::string> bridge,
      std::vector<std::string> component_names);

  static void HandleShutdown(async_dispatcher_t* dispatcher, async_task_t* task,
                             zx_status_t status);

  fpromise::promise<fidl::Client<fuchsia_diagnostics::BatchIterator>> GetBatchIterator(
      fuchsia_diagnostics::DataType data_type, fuchsia_diagnostics::StreamMode stream_mode);

  fpromise::promise<fidl::Client<fuchsia_diagnostics::ArchiveAccessor>> Bind(
      async_dispatcher_t* dispatcher);

  std::optional<fit::function<void(fidl::Client<fuchsia_diagnostics::ArchiveAccessor>&)>> callback_;

  bool creating_archive_ = false;

  void GetArchive(
      fit::function<void(fidl::Client<fuchsia_diagnostics::ArchiveAccessor>&)> callback);

  // Resolved archive if present.
  std::optional<fidl::Client<fuchsia_diagnostics::ArchiveAccessor>> maybe_archive_;

  // The executor on which promise continuations run.
  async::Executor executor_;

  // Thread ID that FIDL objects are bound to. Destruction
  // must happen on this thread.
  std::optional<std::thread::id> thread_id_;

  // The selectors used to filter data streamed from this reader.
  std::vector<std::string> selectors_;

  // The scope to tie async task lifetimes to this object.
  fpromise::scope scope_;

  // Task used to shutdown FIDL objects associated with this object.
  ShutdownTask shutdown_task_;
};

void EmplaceInspect(rapidjson::Document document, std::vector<InspectData>* out);

std::string SanitizeMonikerForSelectors(std::string_view moniker);

}  // namespace diagnostics::reader

#endif  // LIB_DIAGNOSTICS_READER_CPP_ARCHIVE_READER_H_
