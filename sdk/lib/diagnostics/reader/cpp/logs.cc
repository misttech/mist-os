// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <lib/diagnostics/reader/cpp/constants.h>
#include <lib/diagnostics/reader/cpp/logs.h>
#include <lib/fpromise/scope.h>

#include <src/lib/fsl/vmo/strings.h>

namespace diagnostics::reader {

namespace {

inline fuchsia::diagnostics::Severity StringToSeverity(const std::string& input) {
  if (strcasecmp(input.c_str(), "trace") == 0) {
    return fuchsia::diagnostics::Severity::TRACE;
  }
  if (strcasecmp(input.c_str(), "debug") == 0) {
    return fuchsia::diagnostics::Severity::DEBUG;
  }
  if (strcasecmp(input.c_str(), "info") == 0) {
    return fuchsia::diagnostics::Severity::INFO;
  }
  if (strcasecmp(input.c_str(), "warn") == 0) {
    return fuchsia::diagnostics::Severity::WARN;
  }
  if (strcasecmp(input.c_str(), "error") == 0) {
    return fuchsia::diagnostics::Severity::ERROR;
  }
  if (strcasecmp(input.c_str(), "fatal") == 0) {
    return fuchsia::diagnostics::Severity::FATAL;
  }
  // We must never get here as long as we are reading data from Archivist.
  return fuchsia::diagnostics::Severity::INFO;
}

}  // namespace

LogsSubscription::LogsSubscription(fuchsia::diagnostics::BatchIteratorPtr iterator)
    : iterator_(std::move(iterator)), done_(false) {}

bool LogsSubscription::Done() { return done_; }

LogsSubscription::Promise LogsSubscription::Next() {
  return fpromise::make_promise([this] { return ReadBatch(); }).wrap_with(scope_);
}

LogsSubscription::Promise LogsSubscription::ReadBatch() {
  fpromise::bridge<std::optional<LogsData>, std::string> bridge;
  if (!pending_.empty()) {
    auto result = std::make_optional(std::move(pending_.front()));
    pending_.pop();
    return fpromise::make_result_promise<std::optional<LogsData>, std::string>(
        fpromise::ok(std::move(result)));
  } else if (done_) {
    return fpromise::make_result_promise<std::optional<LogsData>, std::string>(
        fpromise::ok(std::nullopt));
  }
  iterator_->GetNext([this, completer = std::move(bridge.completer)](auto result) mutable {
    if (result.is_err()) {
      completer.complete_error("Batch iterator returned error: " +
                               std::to_string(static_cast<size_t>(result.err())));
      return;
    }

    if (result.response().batch.empty()) {
      done_ = true;
      completer.complete_ok(std::nullopt);
      return;
    }

    for (const auto& content : result.response().batch) {
      if (!content.is_json()) {
        completer.complete_error("Received an unexpected content format");
        return;
      }
      std::string json;
      if (!fsl::StringFromVmo(content.json(), &json)) {
        completer.complete_error("Failed to read returned VMO");
        return;
      }
      rapidjson::Document document;
      document.Parse(json);
      completer.complete_ok(LoadJson(std::move(document)));
    }
  });
  return bridge.consumer.promise_or(fpromise::error("Failed to obtain consumer promise"));
}

std::optional<LogsData> LogsSubscription::LoadJson(rapidjson::Document document) {
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
      pending_.push(LogsData(std::move(value_document)));
    }
  } else {
    pending_.push(LogsData(std::move(document)));
  }
  if (pending_.empty()) {
    return std::nullopt;
  }
  auto result = std::make_optional(std::move(pending_.front()));
  pending_.pop();
  return result;
}

LogsData::LogsData(rapidjson::Document document) {
  if (document.HasMember(kMonikerName) && document[kMonikerName].IsString()) {
    std::string val = document[kMonikerName].GetString();
    moniker_ = document[kMonikerName].GetString();
  }
  if (document.HasMember(kVersionName) && document[kVersionName].IsNumber()) {
    version_ = document[kVersionName].GetInt64();
  } else {
    version_ = 0;
  }

  if (document.HasMember(kMetadataName) && document[kMetadataName].IsObject()) {
    const auto& metadata = document[kMetadataName].GetObject();
    if (metadata.HasMember(kMetadataComponentURL) && metadata[kMetadataComponentURL].IsString()) {
      metadata_.component_url = metadata[kMetadataComponentURL].GetString();
    }

    if (metadata.HasMember(kMetadataTimestamp) && metadata[kMetadataTimestamp].IsUint64()) {
      metadata_.timestamp = metadata[kMetadataTimestamp].GetUint64();
    }

    if (metadata.HasMember(kMetadataSeverity) && metadata[kMetadataSeverity].IsString()) {
      metadata_.severity = StringToSeverity(metadata[kMetadataSeverity].GetString());
    }

    if (metadata.HasMember(kMetadataTags) && metadata[kMetadataTags].IsArray()) {
      const auto& tags = metadata[kMetadataTags].GetArray();
      for (auto tag = tags.Begin(); tag != tags.End(); tag++) {
        if (tag->IsString()) {
          metadata_.tags.push_back(tag->GetString());
        }
      }
    }

    if (metadata.HasMember(kMetadataPid) && metadata[kMetadataPid].IsUint64()) {
      metadata_.pid = std::make_optional(metadata[kMetadataPid].GetUint64());
    }

    if (metadata.HasMember(kMetadataTid) && metadata[kMetadataTid].IsUint64()) {
      metadata_.tid = std::make_optional(metadata[kMetadataTid].GetUint64());
    }

    if (metadata.HasMember(kMetadataFile) && metadata[kMetadataFile].IsString()) {
      metadata_.file = std::make_optional(metadata[kMetadataFile].GetString());
    }

    if (metadata.HasMember(kMetadataLine) && metadata[kMetadataLine].IsUint64()) {
      metadata_.line = std::make_optional(metadata[kMetadataLine].GetUint64());
    }

    // TODO(b/300181458): do process errors.
    // TODO(b/300181458): do process key values.
  }

  if (document.HasMember(kPayloadName) && document[kPayloadName].IsObject()) {
    const auto& payload = document[kPayloadName].GetObject();
    if (payload.HasMember(kPayloadRoot) && payload[kPayloadRoot].IsObject()) {
      const auto& root = payload[kPayloadRoot].GetObject();
      if (root.HasMember(kPayloadMessage) && root[kPayloadMessage].IsObject()) {
        const auto& message = root[kPayloadMessage].GetObject();
        if (message.HasMember(kPayloadMessageValue) && message[kPayloadMessageValue].IsString()) {
          message_ = message[kPayloadMessageValue].GetString();
        }
      }
    }
  }
}

}  // namespace diagnostics::reader
