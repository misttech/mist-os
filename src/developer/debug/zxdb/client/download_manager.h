// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_ZXDB_CLIENT_DOWNLOAD_MANAGER_H_
#define SRC_DEVELOPER_DEBUG_ZXDB_CLIENT_DOWNLOAD_MANAGER_H_

#include <map>
#include <memory>

#include "src/developer/debug/zxdb/client/symbol_server.h"
#include "src/developer/debug/zxdb/symbols/debug_symbol_file_type.h"
#include "src/lib/fxl/memory/weak_ptr.h"

namespace zxdb {

class Download;
class System;

// This class manages all downloads that are requested when they are not found locally.
class DownloadManager {
 public:
  explicit DownloadManager(System* system);
  ~DownloadManager();

  // Requests to download |build_id| from any remote source, the first returning a valid symbol file
  // with a matching build id will be loaded. |file_type| determines whether the file should be a
  // binary with symbols or a separate file with a ".debug" suffix with just debug info and no
  // binary information.
  void RequestDownload(const std::string& build_id, DebugSymbolFileType file_type);

  // Adds |server| to the download object associated with any modules that are missing
  // symbols in the current process.
  void OnSymbolServerBecomesReady(SymbolServer* server);

  bool HasDownload(const std::string& build_id) const;

  // Get a test download object.
  std::shared_ptr<Download> InjectDownloadForTesting(const std::string& build_id);

 private:
  // Create a new download object for downloading a given build ID.
  //
  // If multiple callers request a download of the same build ID, this will return the same object
  // to each.
  std::shared_ptr<Download> GetDownload(std::string build_id, DebugSymbolFileType file_type);

  // Called every time a new download starts.
  void DownloadStarted();

  // Called every time a download ends.
  void DownloadFinished();

  size_t download_count_ = 0;

  size_t download_success_count_ = 0;

  size_t download_fail_count_ = 0;

  // Downloads currently in progress.
  std::map<std::pair<std::string, DebugSymbolFileType>, std::weak_ptr<Download>> downloads_;

  System* system_;  // owns |this|.

  fxl::WeakPtrFactory<DownloadManager> weak_factory_;
};

}  // namespace zxdb

#endif  // SRC_DEVELOPER_DEBUG_ZXDB_CLIENT_DOWNLOAD_MANAGER_H_
