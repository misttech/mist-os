// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <lib/fidl/cpp/wire/string_view.h>
#include <lib/fidl/cpp/wire/vector_view.h>

#include <map>
#include <vector>

#include <fbl/string.h>
#include <fbl/string_printf.h>
#include <mock-boot-arguments/server.h>

namespace mock_boot_arguments {

zx::result<fidl::WireSyncClient<fuchsia_boot::Arguments>> Server::CreateClient(
    async_dispatcher_t* dispatcher) {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_boot::Arguments>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  auto& [client, server] = endpoints.value();

  fidl::BindServer(dispatcher, std::move(server), this);

  return zx::ok(fidl::WireSyncClient{std::move(client)});
}

void Server::GetString(GetStringRequestView request, GetStringCompleter::Sync& completer) {
  auto ret = arguments_.find(std::string{request->key.data(), request->key.size()});
  if (ret == arguments_.end()) {
    completer.Reply(fidl::StringView{});
  } else {
    completer.Reply(fidl::StringView::FromExternal(ret->second));
  }
}

void Server::GetStrings(GetStringsRequestView request, GetStringsCompleter::Sync& completer) {
  std::vector<fidl::StringView> result;
  for (uint64_t i = 0; i < request->keys.count(); i++) {
    auto ret = arguments_.find(std::string{request->keys[i].data(), request->keys[i].size()});
    if (ret == arguments_.end()) {
      result.emplace_back();
    } else {
      result.emplace_back(fidl::StringView::FromExternal(ret->second));
    }
  }
  completer.Reply(fidl::VectorView<fidl::StringView>::FromExternal(result));
}

void Server::GetBool(GetBoolRequestView request, GetBoolCompleter::Sync& completer) {
  completer.Reply(StrToBool(request->key, request->defaultval));
}

void Server::GetBools(GetBoolsRequestView request, GetBoolsCompleter::Sync& completer) {
  // The vector<bool> optimisation means we have to use a manually-allocated array.
  std::unique_ptr<bool[]> ret = std::make_unique<bool[]>(request->keys.count());
  for (uint64_t i = 0; i < request->keys.count(); i++) {
    ret[i] = StrToBool(request->keys.data()[i].key, request->keys.data()[i].defaultval);
  }
  completer.Reply(fidl::VectorView<bool>::FromExternal(ret.get(), request->keys.count()));
}

void Server::Collect(CollectRequestView request, CollectCompleter::Sync& completer) {
  std::string match{request->prefix.data(), request->prefix.size()};
  std::vector<fbl::String> result;
  for (auto entry : arguments_) {
    if (entry.first.find_first_of(match) == 0) {
      auto pair = fbl::StringPrintf("%s=%s", entry.first.data(), entry.second.data());
      result.emplace_back(std::move(pair));
    }
  }
  std::vector<fidl::StringView> views;
  views.reserve(result.size());
  for (const auto& val : result) {
    views.emplace_back(fidl::StringView::FromExternal(val));
  }
  completer.Reply(fidl::VectorView<fidl::StringView>::FromExternal(views));
}

bool Server::StrToBool(const fidl::StringView& key, bool defaultval) {
  auto ret = arguments_.find(std::string{key.data(), key.size()});

  if (ret == arguments_.end()) {
    return defaultval;
  }
  if (ret->second == "off" || ret->second == "0" || ret->second == "false") {
    return false;
  }

  return true;
}

}  // namespace mock_boot_arguments
