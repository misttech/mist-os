// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/client/download_manager.h"

#include <lib/syslog/cpp/macros.h>

#include <map>
#include <memory>

#include "src/developer/debug/shared/message_loop.h"
#include "src/developer/debug/zxdb/client/download_observer.h"
#include "src/developer/debug/zxdb/client/process.h"
#include "src/developer/debug/zxdb/client/session.h"
#include "src/developer/debug/zxdb/client/symbol_server.h"
#include "src/developer/debug/zxdb/client/system.h"
#include "src/developer/debug/zxdb/common/err.h"
#include "src/developer/debug/zxdb/symbols/debug_symbol_file_type.h"
#include "src/developer/debug/zxdb/symbols/loaded_module_symbols.h"
#include "src/developer/debug/zxdb/symbols/process_symbols.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace zxdb {

namespace {
std::string DebugSymbolFileTypeToString(DebugSymbolFileType file_type) {
  switch (file_type) {
    case DebugSymbolFileType::kDebugInfo:
      return "debuginfo";
    case DebugSymbolFileType::kBinary:
      return "binary";
  }
}
}  // namespace

// When we want to download symbols for a build ID, we create a Download object. We then fire off
// requests to all symbol servers we know about asking whether they have the symbols we need. These
// requests are async, and the callbacks each own a shared_ptr to the Download object. If all the
// callbacks run and none of them are informed the request was successful, all of the shared_ptrs
// are dropped and the Download object is freed. The destructor of the Download object calls a
// callback that handles notifying the rest of the system of those results.
//
// If one of the callbacks does report that the symbols were found, a transaction to actually start
// the download is initiated, and its reply callback is again given a shared_ptr to the download. If
// we receive more notifications that other servers also have the symbol in the meantime, they are
// queued and will be tried as a fallback if the download fails. Again, once the download callback
// runs the shared_ptr is dropped, and when the Download object dies the destructor handles
// notifying the system.
class Download {
 public:
  Download(const std::string& build_id, DebugSymbolFileType file_type,
           SymbolServer::FetchCallback result_cb)
      : build_id_(build_id), file_type_(file_type), result_cb_(std::move(result_cb)) {}

  ~Download() { Finish(); }

  bool active() { return !!result_cb_; }

  // Add a symbol server to this download.
  void AddServer(std::shared_ptr<Download> self, SymbolServer* server);

 private:
  // FetchFunction is a function that downloads the symbol file from one server.
  // Multiple fetches are queued in server_fetches_ and tried in sequence.
  using FetchFunction = fit::callback<void(SymbolServer::FetchCallback)>;

  // Notify this download object that we have gotten the symbols if we're going to get them.
  void Finish();

  // Notify this Download object that one of the servers has the symbols available.
  void Found(std::shared_ptr<Download> self, FetchFunction fetch);

  // Notify this Download object that a transaction failed.
  void Error(std::shared_ptr<Download> self, const Err& err);

  void RunFetch(std::shared_ptr<Download> self, FetchFunction& fetch);

  size_t failed_server_count_ = 0;
  std::string build_id_;
  DebugSymbolFileType file_type_;
  // Collection for errors encountered during download.
  std::map<ErrType, std::vector<std::string>> errors_;
  std::string path_;
  SymbolServer::FetchCallback result_cb_;
  std::vector<FetchFunction> server_fetches_;
  bool trying_ = false;
};

void Download::Finish() {
  if (!result_cb_)
    return;

  Err final_err;
  if (errors_.size() == 1) {
    final_err = Err(errors_.begin()->first);
  } else if (!errors_.empty()) {
    final_err =
        Err(fxl::StringPrintf("Multiple errors encountered while downloading %s for build_id %s",
                              DebugSymbolFileTypeToString(file_type_).c_str(), build_id_.c_str()));
  }

  // Specialize the error message if the build id was not found on any servers.
  if (final_err.type() == ErrType::kNotFound) {
    final_err = Err("%s for build_id %s not found on %zu servers\n",
                    DebugSymbolFileTypeToString(file_type_).c_str(), build_id_.c_str(),
                    failed_server_count_);
  }

  if (debug::MessageLoop::Current()) {
    debug::MessageLoop::Current()->PostTask(
        FROM_HERE, [result_cb = std::move(result_cb_), final_err,
                    path = std::move(path_)]() mutable { result_cb(final_err, path); });
  }

  result_cb_ = nullptr;
}

void Download::AddServer(std::shared_ptr<Download> self, SymbolServer* server) {
  FX_DCHECK(self.get() == this);

  if (!result_cb_)
    return;

  server->CheckFetch(build_id_, file_type_, [self](const Err& err, FetchFunction fetch) {
    if (!fetch) {
      self->Error(self, err);
    } else {
      self->Found(self, std::move(fetch));
    }
  });
}

void Download::Found(std::shared_ptr<Download> self, FetchFunction fetch) {
  FX_DCHECK(self.get() == this);

  if (!result_cb_)
    return;

  if (trying_) {
    server_fetches_.push_back(std::move(fetch));
    return;
  }

  RunFetch(self, fetch);
}

void Download::Error(std::shared_ptr<Download> self, const Err& err) {
  FX_DCHECK(self.get() == this);

  if (!result_cb_)
    return;

  failed_server_count_++;

  errors_[err.type()].push_back(err.msg());

  if (!trying_ && !server_fetches_.empty()) {
    RunFetch(self, server_fetches_.back());
    server_fetches_.pop_back();
  }
}

void Download::RunFetch(std::shared_ptr<Download> self, FetchFunction& fetch) {
  FX_DCHECK(!trying_);
  trying_ = true;

  fetch([self](const Err& err, const std::string& path) {
    self->trying_ = false;

    if (path.empty()) {
      self->Error(self, err);
    } else {
      self->errors_.clear();
      self->path_ = path;
      self->Finish();
    }
  });
}

// ============================ DownloadManager ===============================

DownloadManager::DownloadManager(System* system) : system_(system), weak_factory_(this) {}

DownloadManager::~DownloadManager() = default;

void DownloadManager::RequestDownload(const std::string& build_id, DebugSymbolFileType file_type) {
  GetDownload(build_id, file_type);
}

std::shared_ptr<Download> DownloadManager::InjectDownloadForTesting(const std::string& build_id) {
  return GetDownload(build_id, DebugSymbolFileType::kDebugInfo);
}

std::shared_ptr<Download> DownloadManager::GetDownload(std::string build_id,
                                                       DebugSymbolFileType file_type) {
  DownloadIdentifier download_id{build_id, file_type};

  if (auto existing = downloads_[download_id].lock()) {
    return existing;
  } else if (failed_downloads_.find(download_id) != failed_downloads_.end()) {
    // This build_id has failed previously. Don't try again.
    return nullptr;
  }

  auto download = std::make_shared<Download>(
      build_id, file_type,
      [build_id, file_type, weak_this = weak_factory_.GetWeakPtr()](const Err& err,
                                                                    const std::string& path) {
        if (!weak_this) {
          return;
        }

        if (!path.empty()) {
          weak_this->download_success_count_++;

          // Adds the file manually since the build_id could already be marked as missing in the
          // build_id_index.
          weak_this->system_->GetSymbols()->build_id_index().AddOneFile(path);

          for (const auto target : weak_this->system_->GetTargets()) {
            if (auto process = target->GetProcess()) {
              // Don't need local symbol lookup when retrying a download, so can pass an empty
              // module name.
              process->GetSymbols()->RetryLoadBuildID(std::string(), build_id, file_type);
            }
          }
        } else {
          weak_this->download_fail_count_++;
          weak_this->failed_downloads_.insert({build_id, file_type});
          weak_this->system_->NotifyFailedToFindDebugSymbols(err, build_id, file_type);
        }

        weak_this->DownloadFinished();
      });

  // Add all configured servers to the new download object.
  DownloadStarted(download);

  downloads_[download_id] = download;

  return download;
}

void DownloadManager::DownloadStarted(const std::shared_ptr<Download>& download) {
  FX_DCHECK(system_);

  for (auto& server : system_->GetSymbolServers()) {
    if (server->state() == SymbolServer::State::kReady)
      download->AddServer(download, server);
  }

  if (download_count_ == 0) {
    for (auto& observer : system_->session()->download_observers()) {
      observer.OnDownloadsStarted();
    }
  }

  download_count_++;
}

void DownloadManager::DownloadFinished() {
  download_count_--;

  FX_DCHECK(system_);
  if (download_count_ == 0) {
    for (auto& observer : system_->session()->download_observers()) {
      observer.OnDownloadsStopped(download_success_count_, download_fail_count_);
    }

    download_success_count_ = download_fail_count_ = 0;
  }
}

bool DownloadManager::HasDownload(const std::string& build_id) const {
  auto download = downloads_.find({build_id, DebugSymbolFileType::kDebugInfo});

  if (download == downloads_.end()) {
    download = downloads_.find({build_id, DebugSymbolFileType::kBinary});
  }

  if (download == downloads_.end()) {
    return false;
  }

  auto ptr = download->second.lock();
  return ptr && ptr->active();
}
}  // namespace zxdb
