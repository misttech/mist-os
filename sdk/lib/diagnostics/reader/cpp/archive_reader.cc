// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/diagnostics/reader/cpp/archive_reader.h>
#include <lib/diagnostics/reader/cpp/constants.h>
#include <lib/diagnostics/reader/cpp/inspect.h>
#include <lib/fpromise/bridge.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <unistd.h>

#include <algorithm>
#include <iostream>
#include <memory>
#include <set>
#include <string>
#include <thread>
#include <vector>

#include <rapidjson/document.h>
#include <rapidjson/rapidjson.h>
#include <src/lib/fsl/vmo/strings.h>
#include <src/lib/fxl/strings/join_strings.h>

#include "lib/async/sequence_id.h"
#include "lib/async/task.h"
#include "lib/fpromise/promise.h"
#include "lib/sync/completion.h"

namespace diagnostics::reader {

// Time to delay between snapshots to find components.
// 250ms so that tests are not overly delayed. Missing the component at
// first is common since the system needs time to start it and receive
// the events.
constexpr size_t kDelayMs = 250;

void EmplaceInspect(rapidjson::Document document, std::vector<InspectData>* out) {
  if (document.IsArray()) {
    for (auto& value : document.GetArray()) {
      // We need to ensure that the value is safely moved between documents, which may involve
      // copying.
      //
      // It is an error to maintain a reference to a Value in a Document after that Document is
      // destroyed, and the input |document| is destroyed immediately after this branch.
      rapidjson::Document value_document;
      rapidjson::Value temp(value.Move(), value_document.GetAllocator());
      value_document.Swap(temp);
      out->emplace_back(InspectData(std::move(value_document)));
    }
  } else {
    out->emplace_back(InspectData(std::move(document)));
  }
}

namespace {

void InnerReadBatches(fidl::Client<fuchsia_diagnostics::BatchIterator> ptr,
                      fpromise::bridge<std::vector<InspectData>, std::string> done,
                      std::vector<InspectData> ret) {
  ptr->GetNext().Then(
      [ptr = std::move(ptr), done = std::move(done), ret = std::move(ret)](auto& result) mutable {
        if (result.is_error()) {
          done.completer.complete_error("Batch iterator returned error: " +
                                        result.error_value().FormatDescription());
          return;
        }

        if (result->batch().empty()) {
          done.completer.complete_ok(std::move(ret));
          return;
        }

        for (auto& content : result->batch()) {
          if (content.Which() != fuchsia_diagnostics::FormattedContent::Tag::kJson) {
            done.completer.complete_error("Received an unexpected content format");
            return;
          }
          std::string json;
          fsl::SizedVmo vmo(std::move(content.json()->vmo()), content.json()->size());
          if (!fsl::StringFromVmo(vmo, &json)) {
            done.completer.complete_error("Failed to read returned VMO");
            return;
          }
          rapidjson::Document document;
          document.Parse(json);

          EmplaceInspect(std::move(document), &ret);
        }

        InnerReadBatches(std::move(ptr), std::move(done), std::move(ret));
      });
}

fpromise::promise<std::vector<InspectData>, std::string> ReadBatches(
    fidl::Client<fuchsia_diagnostics::BatchIterator> ptr) {
  fpromise::bridge<std::vector<InspectData>, std::string> result;
  auto consumer = std::move(result.consumer);
  InnerReadBatches(std::move(ptr), std::move(result), {});
  return consumer.promise_or(fpromise::error("Failed to obtain consumer promise"));
}

}  // namespace

// TODO(b/303304683): This must become the primary API. Currently it is delegating to the
// deprecated FIDL system.
ArchiveReader::ArchiveReader(async_dispatcher_t* dispatcher, std::vector<std::string> selectors)
    : executor_(dispatcher), selectors_(std::move(selectors)) {
  ZX_ASSERT(dispatcher != nullptr);
}

fpromise::promise<fidl::Client<fuchsia_diagnostics::ArchiveAccessor>> ArchiveReader::Bind(
    async_dispatcher_t* dispatcher) {
  return fpromise::make_promise([dispatcher] {
    auto archive = component::Connect<fuchsia_diagnostics::ArchiveAccessor>();
    ZX_ASSERT(archive.is_ok());
    fidl::Client<fuchsia_diagnostics::ArchiveAccessor> old;
    old.Bind(std::move(archive.value()), dispatcher);
    return fpromise::ok(std::move(old));
  });
}

fpromise::promise<std::vector<InspectData>, std::string> ArchiveReader::GetInspectSnapshot() {
  fpromise::bridge<std::vector<InspectData>, std::string> bridge;
  GetArchive([this, completer = std::move(bridge.completer)](
                 fidl::Client<fuchsia_diagnostics::ArchiveAccessor>& archive) mutable {
    std::vector<fuchsia_diagnostics::SelectorArgument> selector_args;
    for (const auto& selector : selectors_) {
      auto arg = fuchsia_diagnostics::SelectorArgument::WithRawSelector(selector);
      selector_args.emplace_back(std::move(arg));
    }

    fuchsia_diagnostics::StreamParameters params;
    params.data_type(fuchsia_diagnostics::DataType::kInspect);
    params.stream_mode(fuchsia_diagnostics::StreamMode::kSnapshot);
    params.format(fuchsia_diagnostics::Format::kJson);

    if (!selector_args.empty()) {
      params.client_selector_configuration(
          fuchsia_diagnostics::ClientSelectorConfiguration::WithSelectors(
              std::move(selector_args)));
    } else {
      params.client_selector_configuration(
          fuchsia_diagnostics::ClientSelectorConfiguration::WithSelectAll(true));
    }

    auto endpoints = fidl::CreateEndpoints<fuchsia_diagnostics::BatchIterator>();
    fidl::Request<fuchsia_diagnostics::ArchiveAccessor::StreamDiagnostics> request;
    request.result_stream(std::move(endpoints->server));
    request.stream_parameters(std::move(params));
    archive->StreamDiagnostics(std::move(request)).is_ok();
    fidl::Client<fuchsia_diagnostics::BatchIterator> client;
    client.Bind(std::move(endpoints->client), this->executor_.dispatcher());
    executor_.schedule_task(
        ReadBatches(std::move(client))
            .then([completer = std::move(completer)](
                      fpromise::result<std::vector<InspectData>, std::string>& result) mutable {
              if (result.is_ok()) {
                completer.complete_ok(result.take_value());
              } else {
                completer.complete_error(result.take_error());
              }
            }));
  });
  return bridge.consumer.promise_or(fpromise::error("Failed to obtain consumer promise"));
}

fpromise::promise<std::vector<InspectData>, std::string> ArchiveReader::SnapshotInspectUntilPresent(
    std::vector<std::string> monikers) {
  fpromise::bridge<std::vector<InspectData>, std::string> bridge;

  InnerSnapshotInspectUntilPresent(std::move(bridge.completer), std::move(monikers));

  return bridge.consumer.promise_or(fpromise::error("Failed to create bridge promise"));
}

void ArchiveReader::GetArchive(
    fit::function<void(fidl::Client<fuchsia_diagnostics::ArchiveAccessor>&)> callback) {
  if (maybe_archive_.has_value()) {
    executor_.schedule_task(fpromise::make_promise(
        [callback = std::move(callback), this]() mutable { callback(*maybe_archive_); }));
    return;
  }
  if (callback_.has_value()) {
    // Chain callbacks
    auto prev = std::move(*callback_);
    callback_ = [prev = std::move(prev), next = std::move(callback)](
                    fidl::Client<fuchsia_diagnostics::ArchiveAccessor>& client) {
      prev(client);
      next(client);
    };
    return;
  }
  // Set first callback
  callback_ = std::move(callback);

  if (!creating_archive_) {
    creating_archive_ = true;
    executor_.schedule_task(
        Bind(executor_.dispatcher())
            .and_then([this](fidl::Client<fuchsia_diagnostics::ArchiveAccessor>& client) {
              thread_id_ = std::this_thread::get_id();
              maybe_archive_ = std::move(client);
              if (callback_.has_value()) {
                auto callback = std::move(*callback_);
                callback(*maybe_archive_);
              }
            }));
  }
}

void ArchiveReader::HandleShutdown(async_dispatcher_t* dispatcher, async_task_t* task,
                                   zx_status_t status) {
  sync_completion_signal(&static_cast<ShutdownTask*>(task)->completion);
}

ArchiveReader::~ArchiveReader() {
  if (!thread_id_.has_value()) {
    // No FIDL resources were created, shutdown
    // is safe to do on the current thread.
    return;
  }
  if (*thread_id_ == std::this_thread::get_id()) {
    // Thread IDs match, shutdown on the current thread is safe.
    return;
  }

  // Thread IDs don't match, shutdown must happen on the dispatcher thread.
  executor_.schedule_task(fpromise::make_promise([&]() {
    // Ensure destructor is called
    { auto client = std::move(maybe_archive_); }
    shutdown_task_.deadline = async_now(executor_.dispatcher());
    shutdown_task_.state = ASYNC_STATE_INIT;
    shutdown_task_.handler = HandleShutdown;
    // Post final shutdown task to wake the other thread. This task must have exited
    // before the rest of the shutdown finishes.
    async_post_task(executor_.dispatcher(), &shutdown_task_);
  }));
  sync_completion_wait(&shutdown_task_.completion, ZX_TIME_INFINITE);
}

void ArchiveReader::InnerSnapshotInspectUntilPresent(
    fpromise::completer<std::vector<InspectData>, std::string> completer,
    std::vector<std::string> monikers) {
  executor_.schedule_task(
      GetInspectSnapshot()
          .then([this, monikers = std::move(monikers), completer = std::move(completer)](
                    fpromise::result<std::vector<InspectData>, std::string>& result) mutable {
            if (result.is_error()) {
              completer.complete_error(result.take_error());
              return;
            }

            auto value = result.take_value();
            std::set<std::string> remaining(monikers.begin(), monikers.end());
            for (const auto& val : value) {
              remaining.erase(val.moniker());
            }

            if (remaining.empty()) {
              completer.complete_ok(std::move(value));
            } else {
              fpromise::bridge<> timeout;
              async::PostDelayedTask(
                  executor_.dispatcher(),
                  [completer = std::move(timeout.completer)]() mutable { completer.complete_ok(); },
                  zx::msec(kDelayMs));
              executor_.schedule_task(
                  timeout.consumer.promise_or(fpromise::error())
                      .then([this, completer = std::move(completer),
                             monikers = std::move(monikers)](fpromise::result<>& res) mutable {
                        InnerSnapshotInspectUntilPresent(std::move(completer), std::move(monikers));
                      })
                      .wrap_with(scope_));
            }
          })
          .wrap_with(scope_));
}

LogsSubscription ArchiveReader::GetLogs(fuchsia_diagnostics::StreamMode mode) {
  auto iterator = GetBatchIterator(fuchsia_diagnostics::DataType::kLogs, std::move(mode));
  return LogsSubscription(std::move(iterator), executor_);
}

fpromise::promise<fidl::Client<fuchsia_diagnostics::BatchIterator>> ArchiveReader::GetBatchIterator(
    fuchsia_diagnostics::DataType data_type, fuchsia_diagnostics::StreamMode stream_mode) {
  fpromise::bridge<fidl::Client<fuchsia_diagnostics::BatchIterator>> bridge;
  GetArchive([this, data_type, stream_mode, completer = std::move(bridge.completer)](
                 fidl::Client<fuchsia_diagnostics::ArchiveAccessor>& archive) mutable {
    std::vector<fuchsia_diagnostics::SelectorArgument> selector_args;
    for (const auto& selector : selectors_) {
      auto arg = fuchsia_diagnostics::SelectorArgument::WithRawSelector(selector);
      selector_args.emplace_back(std::move(arg));
    }

    fuchsia_diagnostics::StreamParameters params;
    params.data_type(data_type);
    params.stream_mode(stream_mode);
    params.format(fuchsia_diagnostics::Format::kJson);

    if (!selector_args.empty()) {
      auto client_selector_config =
          fuchsia_diagnostics::ClientSelectorConfiguration::WithSelectors(std::move(selector_args));
      params.client_selector_configuration(std::move(client_selector_config));
    } else {
      auto client_selector_config =
          fuchsia_diagnostics::ClientSelectorConfiguration::WithSelectAll(true);
      params.client_selector_configuration(std::move(client_selector_config));
    }
    auto endpoints = fidl::CreateEndpoints<fuchsia_diagnostics::BatchIterator>();
    fidl::Request<fuchsia_diagnostics::ArchiveAccessor::StreamDiagnostics> request;
    request.result_stream(std::move(endpoints->server));
    request.stream_parameters(std::move(params));
    archive->StreamDiagnostics(std::move(request)).is_ok();
    fidl::Client<fuchsia_diagnostics::BatchIterator> client;
    client.Bind(std::move(endpoints->client), this->executor_.dispatcher());
    completer.complete_ok(std::move(client));
  });
  return bridge.consumer.promise();
}

std::string SanitizeMonikerForSelectors(std::string_view moniker) {
  std::string result(moniker);
  const size_t ESCAPED_LEN = 2;
  const size_t COLON_LEN = 1;
  size_t index = 0;
  while ((index = result.find(":", index)) != std::string::npos) {
    result.replace(index, COLON_LEN, "\\:");
    index += ESCAPED_LEN;
  }
  return result;
}

}  // namespace diagnostics::reader
