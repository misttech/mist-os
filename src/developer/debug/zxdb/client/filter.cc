// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/client/filter.h"

#include <string_view>

#include "lib/syslog/cpp/macros.h"
#include "src/developer/debug/ipc/filter_utils.h"
#include "src/developer/debug/ipc/records.h"
#include "src/developer/debug/zxdb/client/session.h"
#include "src/developer/debug/zxdb/client/setting_schema.h"
#include "src/developer/debug/zxdb/client/setting_schema_definition.h"

namespace {
uint32_t next_filter_id = 1;
}  // namespace

namespace zxdb {

const char* ClientSettings::Filter::kType = "type";
const char* ClientSettings::Filter::kTypeDescription =
    R"(  The type of the filter. Could be "process name substr", "process name",
  "component name", "component url" or, "component moniker".)";

const char* ClientSettings::Filter::kPattern = "pattern";
const char* ClientSettings::Filter::kPatternDescription =
    R"(  The pattern used for matching. See "help attach" for more help.)";

const char* ClientSettings::Filter::kJob = "job";
const char* ClientSettings::Filter::kJobDescription =
    R"(  The scope of the filter. Only valid when the type is "process name substr" or
  "process name".)";

const char* ClientSettings::Filter::kWeak = "weak";
const char* ClientSettings::Filter::kWeakDescription =
    R"(  Whether or not this is a weak filter. When matched, a weak filter will
  configure the corresponding target to attach but defer loading symbols for
  the matched process only when an exception or breakpoint is raised for that
  process in the backend.)";

const char* ClientSettings::Filter::kRecursive = "recursive";
const char* ClientSettings::Filter::kRecursiveDescription =
    R"(  Whether or not this is a recursive filter. When matched, a recursive
  filter will match all child monikers of this filter. Therefore, the recursive
  option is only valid with component moniker filters.
  )";

const char* ClientSettings::Filter::kJobOnly = "job-only";
const char* ClientSettings::Filter::kJobOnlyDescription =
    R"(  Whether or not this is a job-only filter. When matched, this filter
  will only attach to the parent job of the match's (or matches') process.
  This allows for processes to claim their own exception channels without
  interference from the backend. Exceptions that are not handled by the
  process's exception channel will be reported by the backend, but cannot be
  interactively debugged.)";

namespace {

fxl::RefPtr<SettingSchema> CreateSchema() {
  auto schema = fxl::MakeRefCounted<SettingSchema>();
  schema->AddString(
      ClientSettings::Filter::kType, ClientSettings::Filter::kTypeDescription,
      debug_ipc::Filter::TypeToString(debug_ipc::Filter::Type::kUnset),
      {debug_ipc::Filter::TypeToString(debug_ipc::Filter::Type::kProcessNameSubstr),
       debug_ipc::Filter::TypeToString(debug_ipc::Filter::Type::kProcessName),
       debug_ipc::Filter::TypeToString(debug_ipc::Filter::Type::kComponentName),
       debug_ipc::Filter::TypeToString(debug_ipc::Filter::Type::kComponentUrl),
       debug_ipc::Filter::TypeToString(debug_ipc::Filter::Type::kComponentMoniker),
       debug_ipc::Filter::TypeToString(debug_ipc::Filter::Type::kComponentMonikerSuffix)});
  schema->AddString(ClientSettings::Filter::kPattern, ClientSettings::Filter::kPatternDescription);
  schema->AddInt(ClientSettings::Filter::kJob, ClientSettings::Filter::kJobDescription);
  schema->AddBool(ClientSettings::Filter::kWeak, ClientSettings::Filter::kWeakDescription);
  schema->AddBool(ClientSettings::Filter::kRecursive,
                  ClientSettings::Filter::kRecursiveDescription);
  schema->AddBool(ClientSettings::Filter::kJobOnly, ClientSettings::Filter::kJobOnlyDescription);
  return schema;
}

debug_ipc::Filter::Type StringToType(std::string_view string) {
  for (int i = 0; i < static_cast<int>(debug_ipc::Filter::Type::kLast); i++) {
    auto type = static_cast<debug_ipc::Filter::Type>(i);
    if (string == debug_ipc::Filter::TypeToString(type))
      return type;
  }
  // Input should have been validated.
  FX_NOTREACHED();
  return debug_ipc::Filter::Type::kUnset;
}

}  // namespace

Filter::Settings::Settings(Filter* filter) : SettingStore(Filter::GetSchema()), filter_(filter) {}

SettingValue Filter::Settings::GetStorageValue(const std::string& key) const {
  if (key == ClientSettings::Filter::kType)
    return SettingValue(debug_ipc::Filter::TypeToString(filter_->filter_.type));
  if (key == ClientSettings::Filter::kPattern)
    return SettingValue(filter_->filter_.pattern);
  if (key == ClientSettings::Filter::kJob)
    return SettingValue(static_cast<int64_t>(filter_->filter_.job_koid));
  if (key == ClientSettings::Filter::kWeak)
    return SettingValue(filter_->filter_.config.weak);
  if (key == ClientSettings::Filter::kRecursive)
    return SettingValue(filter_->filter_.config.recursive);
  if (key == ClientSettings::Filter::kJobOnly)
    return SettingValue(filter_->filter_.config.job_only);
  return SettingValue();
}

Err Filter::Settings::SetStorageValue(const std::string& key, SettingValue value) {
  // Schema should have been validated before here.
  if (key == ClientSettings::Filter::kType) {
    filter_->SetType(StringToType(value.get_string()));
  } else if (key == ClientSettings::Filter::kPattern) {
    filter_->SetPattern(value.get_string());
  } else if (key == ClientSettings::Filter::kJob) {
    if (filter_->filter_.type != debug_ipc::Filter::Type::kProcessNameSubstr &&
        filter_->filter_.type != debug_ipc::Filter::Type::kProcessName) {
      return Err("This filter type cannot be associated with a job.");
    }
    filter_->SetJobKoid(value.get_int());
  } else if (key == ClientSettings::Filter::kWeak) {
    filter_->SetWeak(value.get_bool());
  } else if (key == ClientSettings::Filter::kRecursive) {
    filter_->SetRecursive(value.get_bool());
  } else if (key == ClientSettings::Filter::kJobOnly) {
    filter_->SetJobOnly(value.get_bool());
  }
  return Err();
}

Filter::Filter(Session* session) : ClientObject(session), settings_(this) {
  filter_.id = next_filter_id++;
  // The DebugAgent FIDL server reserves the highest bit to differentiate filters that it installs
  // from the ones a zxdb user installed. In general, these two systems are not being used in the
  // same DebugAgent instance at the same time, and 2^31 is a lot of filters, so we really should
  // never conflict.
  //
  // A more robust system would have the backend provision the ids and vend them back out to the
  // clients so this problem doesn't exist, but that would require many changes to the UpdateFilters
  // debug_ipc method that aren't necessary for this simple usecase.
  FX_DCHECK((filter_.id & (1 << 31)) == 0)
      << "Filter ID created in zxdb conflicts with FIDL filters. Please file a bug: "
         "https://fxbug.dev/issues/new?component=1389559&template=1849567.";
}

void Filter::SetType(debug_ipc::Filter::Type type) {
  filter_.type = type;
  Sync();
}

void Filter::SetPattern(const std::string& pattern) {
  filter_.pattern = pattern;
  Sync();
}

void Filter::SetJobKoid(zx_koid_t job_koid) {
  filter_.job_koid = job_koid;
  Sync();
}

void Filter::SetWeak(bool weak) {
  filter_.config.weak = weak;
  Sync();
}

void Filter::SetRecursive(bool recursive) {
  filter_.config.recursive = recursive;
  Sync();
}

void Filter::SetJobOnly(bool job_only) {
  filter_.config.job_only = job_only;
  Sync();
}

bool Filter::ShouldDeferModuleLoading() const { return debug_ipc::FilterDefersModules(&filter_); }

void Filter::Sync() { session()->system().SyncFilters(); }

// static
fxl::RefPtr<SettingSchema> Filter::GetSchema() {
  static fxl::RefPtr<SettingSchema> schema = CreateSchema();
  return schema;
}

}  // namespace zxdb
