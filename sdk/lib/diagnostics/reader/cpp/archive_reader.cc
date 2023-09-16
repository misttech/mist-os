// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <lib/async/cpp/task.h>
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

void InnerReadBatches(fuchsia::diagnostics::BatchIteratorPtr ptr,
                      fpromise::bridge<std::vector<InspectData>, std::string> done,
                      std::vector<InspectData> ret) {
  ptr->GetNext(
      [ptr = std::move(ptr), done = std::move(done), ret = std::move(ret)](auto result) mutable {
        if (result.is_err()) {
          done.completer.complete_error("Batch iterator returned error: " +
                                        std::to_string(static_cast<size_t>(result.err())));
          return;
        }

        if (result.response().batch.empty()) {
          done.completer.complete_ok(std::move(ret));
          return;
        }

        for (const auto& content : result.response().batch) {
          if (!content.is_json()) {
            done.completer.complete_error("Received an unexpected content format");
            return;
          }
          std::string json;
          if (!fsl::StringFromVmo(content.json(), &json)) {
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
    fuchsia::diagnostics::BatchIteratorPtr ptr) {
  fpromise::bridge<std::vector<InspectData>, std::string> result;
  auto consumer = std::move(result.consumer);
  InnerReadBatches(std::move(ptr), std::move(result), {});
  return consumer.promise_or(fpromise::error("Failed to obtain consumer promise"));
}

}  // namespace

ArchiveReader::ArchiveReader(fuchsia::diagnostics::ArchiveAccessorPtr archive,
                             std::vector<std::string> selectors)

    : archive_(std::move(archive)),
      executor_(archive_.dispatcher()),
      selectors_(std::move(selectors)) {
  ZX_ASSERT(archive_.dispatcher() != nullptr);
}

fpromise::promise<std::vector<InspectData>, std::string> ArchiveReader::GetInspectSnapshot() {
  return fpromise::make_promise([this] {
           std::vector<fuchsia::diagnostics::SelectorArgument> selector_args;
           for (const auto& selector : selectors_) {
             fuchsia::diagnostics::SelectorArgument arg;
             arg.set_raw_selector(selector);
             selector_args.emplace_back(std::move(arg));
           }

           fuchsia::diagnostics::StreamParameters params;
           params.set_data_type(fuchsia::diagnostics::DataType::INSPECT);
           params.set_stream_mode(fuchsia::diagnostics::StreamMode::SNAPSHOT);
           params.set_format(fuchsia::diagnostics::Format::JSON);

           fuchsia::diagnostics::ClientSelectorConfiguration client_selector_config;
           if (!selector_args.empty()) {
             client_selector_config.set_selectors(std::move(selector_args));
           } else {
             client_selector_config.set_select_all(true);
           }

           params.set_client_selector_configuration(std::move(client_selector_config));

           fuchsia::diagnostics::BatchIteratorPtr iterator;
           archive_->StreamDiagnostics(std::move(params),
                                       iterator.NewRequest(archive_.dispatcher()));
           return ReadBatches(std::move(iterator));
         })
      .wrap_with(scope_);
}

fpromise::promise<std::vector<InspectData>, std::string> ArchiveReader::SnapshotInspectUntilPresent(
    std::vector<std::string> monikers) {
  fpromise::bridge<std::vector<InspectData>, std::string> bridge;

  InnerSnapshotInspectUntilPresent(std::move(bridge.completer), std::move(monikers));

  return bridge.consumer.promise_or(fpromise::error("Failed to create bridge promise"));
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
              // TODO(fxb/77979) This is a workaround to make tests pass during
              // the migration. Remove after migration
              auto name = val.moniker().substr(val.moniker().find_last_of("/") + 1);
              remaining.erase(name);
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

LogsSubscription ArchiveReader::GetLogs(fuchsia::diagnostics::StreamMode mode) {
  auto iterator = GetBatchIterator(fuchsia::diagnostics::DataType::LOGS, std::move(mode));
  return LogsSubscription(std::move(iterator));
}

fuchsia::diagnostics::BatchIteratorPtr ArchiveReader::GetBatchIterator(
    fuchsia::diagnostics::DataType data_type, fuchsia::diagnostics::StreamMode stream_mode) {
  std::vector<fuchsia::diagnostics::SelectorArgument> selector_args;
  for (const auto& selector : selectors_) {
    fuchsia::diagnostics::SelectorArgument arg;
    arg.set_raw_selector(selector);
    selector_args.emplace_back(std::move(arg));
  }

  fuchsia::diagnostics::StreamParameters params;
  params.set_data_type(std::move(data_type));
  params.set_stream_mode(std::move(stream_mode));
  params.set_format(fuchsia::diagnostics::Format::JSON);

  fuchsia::diagnostics::ClientSelectorConfiguration client_selector_config;
  if (!selector_args.empty()) {
    client_selector_config.set_selectors(std::move(selector_args));
  } else {
    client_selector_config.set_select_all(true);
  }

  params.set_client_selector_configuration(std::move(client_selector_config));

  fuchsia::diagnostics::BatchIteratorPtr iterator;
  archive_->StreamDiagnostics(std::move(params), iterator.NewRequest(archive_.dispatcher()));
  return iterator;
}

}  // namespace diagnostics::reader
