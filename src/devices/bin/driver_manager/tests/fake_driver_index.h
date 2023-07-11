// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_TESTS_FAKE_DRIVER_INDEX_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_TESTS_FAKE_DRIVER_INDEX_H_

#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.driver.index/cpp/fidl.h>
#include <lib/fit/function.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <unordered_set>

class FakeDriverIndex final : public fidl::WireServer<fuchsia_driver_index::DriverIndex> {
 public:
  struct MatchResult {
    std::string url;
    std::optional<fuchsia_driver_index::MatchedCompositeNodeSpecInfo> spec;
    bool is_fallback = false;
    bool colocate = false;
  };

  using MatchCallback =
      fit::function<zx::result<MatchResult>(fuchsia_driver_index::wire::MatchDriverArgs args)>;

  FakeDriverIndex(async_dispatcher_t* dispatcher, MatchCallback match_callback)
      : dispatcher_(dispatcher), match_callback_(std::move(match_callback)) {}

  zx::result<fidl::ClientEnd<fuchsia_driver_index::DriverIndex>> Connect() {
    auto endpoints = fidl::CreateEndpoints<fuchsia_driver_index::DriverIndex>();
    if (endpoints.is_error()) {
      return zx::error(endpoints.status_value());
    }
    fidl::BindServer(dispatcher_, std::move(endpoints->server), this);
    return zx::ok(std::move(endpoints->client));
  }

  void MatchDriver(MatchDriverRequestView request, MatchDriverCompleter::Sync& completer) override {
    auto match = match_callback_(request->args);
    if (match.status_value() != ZX_OK) {
      completer.ReplyError(match.status_value());
      return;
    }

    if (disabled_driver_urls_.find(match->url) != disabled_driver_urls_.end()) {
      completer.ReplyError(ZX_ERR_NOT_FOUND);
      return;
    }

    fidl::Arena arena;
    completer.ReplySuccess(GetMatchedDriver(arena, match.value()));
  }

  void WaitForBaseDrivers(WaitForBaseDriversCompleter::Sync& completer) override {
    completer.Reply();
  }

  void AddCompositeNodeSpec(AddCompositeNodeSpecRequestView request,
                            AddCompositeNodeSpecCompleter::Sync& completer) override {
    auto name = std::string(request->name().get());
    if (spec_match_.find(name) == spec_match_.end()) {
      completer.ReplyError(ZX_ERR_NOT_FOUND);
      return;
    }

    auto matched_result = spec_match_[name];
    auto composite = matched_result.composite();
    auto names = matched_result.node_names();
    if (!composite.has_value() || !names.has_value()) {
      completer.ReplyError(ZX_ERR_INVALID_ARGS);
      return;
    }
    fidl::Arena arena;
    completer.ReplySuccess(fidl::ToWire(arena, composite.value()),
                           fidl::ToWire(arena, names.value()));
  }

  void AddCompositeNodeSpecMatch(std::string_view name,
                                 fuchsia_driver_index::MatchedCompositeNodeSpecInfo result) {
    spec_match_[std::string(name)] = result;
  }

  void set_match_callback(MatchCallback match_callback) {
    match_callback_ = std::move(match_callback);
  }

  void disable_driver_url(std::string_view url) { disabled_driver_urls_.emplace(url); }

  size_t un_disable_driver_url(std::string_view url) {
    return disabled_driver_urls_.erase(std::string(url));
  }

 private:
  static fuchsia_driver_index::wire::MatchedDriver GetMatchedDriver(fidl::AnyArena& arena,
                                                                    MatchResult match) {
    if (match.spec) {
      fuchsia_driver_index::MatchedCompositeNodeParentInfo const result(
          {.specs = std::vector<fuchsia_driver_index::MatchedCompositeNodeSpecInfo>{
               match.spec.value()}});
      return fuchsia_driver_index::wire::MatchedDriver::WithParentSpec(arena,
                                                                       fidl::ToWire(arena, result));
    }

    auto driver_info = GetDriverInfo(arena, match);
    return fuchsia_driver_index::wire::MatchedDriver::WithDriver(
        fidl::ObjectView<fuchsia_driver_index::wire::MatchedDriverInfo>(arena, driver_info));
  }

  static fuchsia_driver_index::wire::MatchedDriverInfo GetDriverInfo(fidl::AnyArena& arena,
                                                                     MatchResult match) {
    return fuchsia_driver_index::wire::MatchedDriverInfo::Builder(arena)
        .driver_url(fidl::ObjectView<fidl::StringView>(arena, arena, match.url))
        .url(fidl::ObjectView<fidl::StringView>(arena, arena, match.url))
        .is_fallback(match.is_fallback)
        .colocate(match.colocate)
        .package_type(fuchsia_driver_index::DriverPackageType::kBoot)
        .Build();
  }

  async_dispatcher_t* dispatcher_;
  MatchCallback match_callback_;

  // Maps a MatchedCompositeNodeSpecInfo to a composite node spec topological path. This gets
  // returned when FakeDriverIndex receives an AddCompositeNodeSpec() call for a matching
  // topological path.
  std::unordered_map<std::string, fuchsia_driver_index::MatchedCompositeNodeSpecInfo> spec_match_;

  std::unordered_set<std::string> disabled_driver_urls_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_TESTS_FAKE_DRIVER_INDEX_H_
