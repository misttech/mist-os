// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <lib/async/cpp/task.h>
#include <lib/fpromise/bridge.h>
#include <lib/inspect/contrib/cpp/archive_reader.h>
#include <unistd.h>

#include <algorithm>
#include <iostream>
#include <memory>
#include <optional>
#include <set>
#include <stack>
#include <string>
#include <thread>
#include <vector>

#include <rapidjson/prettywriter.h>
#include <rapidjson/stringbuffer.h>
#include <src/lib/fsl/vmo/strings.h>
#include <src/lib/fxl/strings/join_strings.h>

#include "lib/inspect/cpp/hierarchy.h"
#include "rapidjson/document.h"
#include "rapidjson/rapidjson.h"

namespace inspect {
namespace contrib {

constexpr char kPathName[] = "moniker";
constexpr char kContentsName[] = "payload";
constexpr char kVersionName[] = "version";
constexpr char kMetadataName[] = "metadata";
constexpr char kMetadataFilename[] = "filename";
constexpr char kMetadataComponentURL[] = "component_url";
constexpr char kMetadataTimestamp[] = "timestamp";
constexpr char kMetadataErrors[] = "errors";
constexpr char kMetadataErrorsMessage[] = "message";

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

bool AllDigits(const std::string& value) {
  for (char c : value) {
    if (!std::isdigit(c)) {
      return false;
    }
  }
  return !value.empty();
}

void SortObject(rapidjson::Value* object) {
  std::sort(object->MemberBegin(), object->MemberEnd(),
            [](const rapidjson::Value::Member& lhs, const rapidjson::Value::Member& rhs) {
              auto lhs_name = lhs.name.GetString();
              auto rhs_name = rhs.name.GetString();
              if (AllDigits(lhs_name) && AllDigits(rhs_name)) {
                uint64_t lhs_val = atoll(lhs_name);
                uint64_t rhs_val = atoll(rhs_name);
                return lhs_val < rhs_val;
              } else {
                return strcmp(lhs_name, rhs_name) < 0;
              }
            });
}

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

void ParseArray(const std::string& name, const rapidjson::Value::Array& arr, NodeValue* node_ptr) {
  switch (arr.Begin()->GetType()) {
    case rapidjson::kStringType: {
      std::vector<std::string> values;
      for (auto& v : arr) {
        values.emplace_back(v.GetString());
      }
      node_ptr->add_property(
          PropertyValue(name, StringArrayValue(std::move(values), ArrayDisplayFormat::kFlat)));
      break;
    }
    case rapidjson::kNumberType: {
      if (arr.Begin()->IsInt64()) {
        std::vector<std::int64_t> values;
        for (auto& v : arr) {
          values.emplace_back(v.GetInt64());
        }
        node_ptr->add_property(
            PropertyValue(name, IntArrayValue(std::move(values), ArrayDisplayFormat::kFlat)));
      } else if (arr.Begin()->IsUint64()) {
        std::vector<std::uint64_t> values;
        for (auto& v : arr) {
          values.emplace_back(v.GetUint64());
        }
        node_ptr->add_property(
            PropertyValue(name, UintArrayValue(std::move(values), ArrayDisplayFormat::kFlat)));
      } else if (arr.Begin()->IsDouble()) {
        std::vector<double> values;
        for (auto& v : arr) {
          values.emplace_back(v.GetDouble());
        }
        node_ptr->add_property(
            PropertyValue(name, DoubleArrayValue(std::move(values), ArrayDisplayFormat::kFlat)));
      }
      break;
    }
    default:
      break;
  }
}

struct StackItem {
  Hierarchy hierarchy;
  rapidjson::Value::MemberIterator next;
  rapidjson::Value::MemberIterator end;
};

inspect::Hierarchy ParsePayload(std::string name, const rapidjson::Value::Object& obj) {
  std::stack<StackItem> pending;
  Hierarchy root, completedHierarchy;
  root.node_ptr()->set_name(std::move(name));
  pending.push({std::move(root), obj.MemberBegin(), obj.MemberEnd()});

  while (!pending.empty()) {
    auto foundObj = false;
    auto& curr = pending.top();
    for (auto& itr = curr.next; itr != curr.end; itr++) {
      auto valueName = itr->name.GetString();

      if (itr->value.IsObject()) {
        Hierarchy child;
        child.node_ptr()->set_name(valueName);
        pending.push({std::move(child), itr->value.GetObject().MemberBegin(),
                      itr->value.GetObject().MemberEnd()});
        itr++;
        foundObj = true;
        // We first need to take care of the child before we can process the remaining entries
        break;
      }

      // If it's not an object it's a property
      switch (itr->value.GetType()) {
        case rapidjson::kNullType:
          break;
        case rapidjson::kFalseType:
        case rapidjson::kTrueType:
          itr->value.GetBool();
          curr.hierarchy.node_ptr()->add_property(
              PropertyValue(valueName, BoolPropertyValue(itr->value.GetBool())));
          break;
        case rapidjson::kStringType:
          curr.hierarchy.node_ptr()->add_property(
              PropertyValue(valueName, StringPropertyValue(itr->value.GetString())));
          break;
        case rapidjson::kNumberType:
          if (itr->value.IsInt64()) {
            curr.hierarchy.node_ptr()->add_property(
                PropertyValue(valueName, IntPropertyValue(itr->value.GetInt64())));
          } else if (itr->value.IsUint64()) {
            curr.hierarchy.node_ptr()->add_property(
                PropertyValue(valueName, UintPropertyValue(itr->value.GetUint64())));
          } else {
            curr.hierarchy.node_ptr()->add_property(
                PropertyValue(valueName, DoublePropertyValue(itr->value.GetDouble())));
          }
          break;
        case rapidjson::kArrayType:
          ParseArray(valueName, itr->value.GetArray(), curr.hierarchy.node_ptr());
          break;
        default:
          break;
      }
    }
    if (!foundObj) {
      completedHierarchy = std::move(curr.hierarchy);
      pending.pop();
      if (!pending.empty()) {
        pending.top().hierarchy.add_child(std::move(completedHierarchy));
      }
    }
  }
  return completedHierarchy;
}
}  // namespace

InspectData::InspectData(rapidjson::Document document) : document_(std::move(document)) {
  if (document_.HasMember(kPathName) && document_[kPathName].IsString()) {
    std::string val = document_[kPathName].GetString();
    moniker_ = document_[kPathName].GetString();
  }
  if (document_.HasMember(kVersionName) && document_[kVersionName].IsNumber()) {
    version_ = document_[kVersionName].GetInt64();
  } else {
    version_ = 0;
  }

  metadata_.timestamp = 0;
  metadata_.filename = "";
  metadata_.component_url = {};
  metadata_.errors = {};

  if (document_.HasMember(kMetadataName) && document_[kMetadataName].IsObject()) {
    const auto& metadata = document_[kMetadataName].GetObject();
    if (metadata.HasMember(kMetadataFilename) && metadata[kMetadataFilename].IsString()) {
      metadata_.filename = metadata[kMetadataFilename].GetString();
    }
    if (metadata.HasMember(kMetadataComponentURL) && metadata[kMetadataComponentURL].IsString()) {
      metadata_.component_url = metadata[kMetadataComponentURL].GetString();
    }
    if (metadata.HasMember(kMetadataTimestamp) && metadata[kMetadataTimestamp].IsNumber()) {
      metadata_.timestamp = metadata[kMetadataTimestamp].GetUint64();
    }
    if (metadata.HasMember(kMetadataErrors) && metadata[kMetadataErrors].IsArray()) {
      const auto& errors = metadata[kMetadataErrors].GetArray();
      std::vector<InspectError> inspectErrors;
      for (auto error = errors.Begin(); error != errors.End(); error++) {
        if (error->IsObject() && error->GetObject()[kMetadataErrorsMessage].IsString()) {
          InspectError ie = {error->GetObject()[kMetadataErrorsMessage].GetString()};
          inspectErrors.emplace_back(ie);
        }
      }
      metadata_.errors = std::make_optional(inspectErrors);
    }
  }
  if (document_.HasMember(kContentsName) && document_[kContentsName].IsObject()) {
    const auto& payload = document_[kContentsName].GetObject();
    if (payload.MemberBegin() != payload.MemberEnd() && payload.MemberBegin()->value.IsObject()) {
      payload_ = ParsePayload(payload.MemberBegin()->name.GetString(),
                              payload.MemberBegin()->value.GetObject());
    }
  }
}

const rapidjson::Value& InspectData::content() const {
  static rapidjson::Value default_ret;

  if (!document_.IsObject() || !document_.HasMember(kContentsName)) {
    return default_ret;
  }

  return document_[kContentsName];
}

const rapidjson::Value& InspectData::GetByPath(const std::vector<std::string>& path) const {
  static rapidjson::Value default_ret;

  const rapidjson::Value* cur = &content();
  for (size_t i = 0; i < path.size(); i++) {
    if (!cur->IsObject()) {
      return default_ret;
    }

    auto it = cur->FindMember(path[i]);
    if (it == cur->MemberEnd()) {
      return default_ret;
    }

    cur = &it->value;
  }

  return *cur;
}

std::string InspectData::PrettyJson() {
  rapidjson::StringBuffer buffer;
  rapidjson::PrettyWriter<rapidjson::StringBuffer> writer(buffer);
  document_.Accept(writer);
  return buffer.GetString();
}

void InspectData::Sort() {
  std::vector<rapidjson::Value*> pending;
  auto& object = document_;
  pending.push_back(&object);
  while (!pending.empty()) {
    rapidjson::Value* pending_object = pending.back();
    pending.pop_back();
    SortObject(pending_object);
    for (auto m = pending_object->MemberBegin(); m != pending_object->MemberEnd(); ++m) {
      if (m->value.IsObject()) {
        pending.push_back(&m->value);
      }
    }
  }
}

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

}  // namespace contrib
}  // namespace inspect
