// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DIAGNOSTICS_READER_CPP_ARCHIVE_READER_H_
#define LIB_DIAGNOSTICS_READER_CPP_ARCHIVE_READER_H_

#include <fuchsia/diagnostics/cpp/fidl.h>
#include <lib/async/cpp/executor.h>
#include <lib/diagnostics/reader/cpp/inspect.h>
#include <lib/fpromise/bridge.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/scope.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/stdcompat/optional.h>

#include <cstdint>
#include <optional>

#include <rapidjson/document.h>

namespace diagnostics::reader {

// ArchiveReader supports reading Inspect data from an Archive.
class ArchiveReader {
 public:
  // Create a new ArchiveReader.
  //
  // archive: A connected interface pointer to the Archive. Must be bound.
  // selectors: The selectors for data to be returned by this call. Empty means to return all data.
  //
  // Note: This constructor asserts that archive is bound.
  ArchiveReader(fuchsia::diagnostics::ArchiveAccessorPtr archive,
                std::vector<std::string> selectors);

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

 private:
  void InnerSnapshotInspectUntilPresent(
      fpromise::completer<std::vector<InspectData>, std::string> bridge,
      std::vector<std::string> component_names);

  // The pointer to the archive this object is connected to.
  fuchsia::diagnostics::ArchiveAccessorPtr archive_;

  // The executor on which promise continuations run.
  async::Executor executor_;

  // The selectors used to filter data streamed from this reader.
  std::vector<std::string> selectors_;

  // The scope to tie async task lifetimes to this object.
  fpromise::scope scope_;
};

void EmplaceInspect(rapidjson::Document document, std::vector<InspectData>* out);

}  // namespace diagnostics::reader

#endif  // LIB_DIAGNOSTICS_READER_CPP_ARCHIVE_READER_H_
