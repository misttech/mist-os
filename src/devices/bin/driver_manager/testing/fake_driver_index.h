// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_TESTING_FAKE_DRIVER_INDEX_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_TESTING_FAKE_DRIVER_INDEX_H_

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
    std::optional<fuchsia_driver_framework::CompositeParent> spec;
    bool is_fallback = false;
    bool colocate = false;
  };

  using MatchCallback =
      fit::function<zx::result<MatchResult>(fuchsia_driver_index::wire::MatchDriverArgs args)>;

  FakeDriverIndex(async_dispatcher_t* dispatcher, MatchCallback match_callback)
      : dispatcher_(dispatcher), match_callback_(std::move(match_callback)) {}

  fidl::ClientEnd<fuchsia_driver_index::DriverIndex> Connect() {
    auto [client_end, server_end] = fidl::Endpoints<fuchsia_driver_index::DriverIndex>::Create();
    fidl::BindServer(dispatcher_, std::move(server_end), this);
    return std::move(client_end);
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

  void WatchForDriverLoad(WatchForDriverLoadCompleter::Sync& completer) override {
    watch_completer_ = completer.ToAsync();
  }

  void AddCompositeNodeSpec(AddCompositeNodeSpecRequestView request,
                            AddCompositeNodeSpecCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }

  void RebindCompositeNodeSpec(RebindCompositeNodeSpecRequestView request,
                               RebindCompositeNodeSpecCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void InvokeWatchDriverResponse() {
    ZX_ASSERT(watch_completer_.has_value());
    watch_completer_->Reply();
  }

  void set_match_callback(MatchCallback match_callback) {
    match_callback_ = std::move(match_callback);
  }

  void disable_driver_url(std::string_view url) { disabled_driver_urls_.emplace(url); }

  size_t un_disable_driver_url(std::string_view url) {
    return disabled_driver_urls_.erase(std::string(url));
  }

 private:
  static fuchsia_driver_index::wire::MatchDriverResult GetMatchedDriver(fidl::AnyArena& arena,
                                                                        MatchResult match) {
    if (match.spec) {
      return fuchsia_driver_index::wire::MatchDriverResult::WithCompositeParents(
          arena, fidl::ToWire(arena, std::vector<fuchsia_driver_framework::CompositeParent>{
                                         match.spec.value()}));
    }

    auto driver_info = GetDriverInfo(arena, match);
    return fuchsia_driver_index::wire::MatchDriverResult::WithDriver(arena, driver_info);
  }

  static fuchsia_driver_framework::wire::DriverInfo GetDriverInfo(fidl::AnyArena& arena,
                                                                  MatchResult match) {
    return fuchsia_driver_framework::wire::DriverInfo::Builder(arena)
        .url(fidl::ObjectView<fidl::StringView>(arena, arena, match.url))
        .is_fallback(match.is_fallback)
        .colocate(match.colocate)
        .package_type(fuchsia_driver_framework::DriverPackageType::kBoot)
        .Build();
  }

  async_dispatcher_t* dispatcher_;
  MatchCallback match_callback_;

  std::optional<WatchForDriverLoadCompleter::Async> watch_completer_;

  std::unordered_set<std::string> disabled_driver_urls_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_TESTING_FAKE_DRIVER_INDEX_H_
