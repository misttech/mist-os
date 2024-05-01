// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/controller_allowlist_passthrough.h"

#include <zircon/assert.h>

#include <memory>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <utility>

#include "lib/fidl/cpp/wire/internal/transport.h"
#include "src/devices/lib/log/log.h"
#include "zircon/errors.h"

namespace {

const char *kAllowAllUses = "Allow_all_uses";

// Listed below are all the class names of _servers_ of the fuchsia.device/Controller protocol
// that are allowed for each function.  There can be and often are multiple clients that use
// a function of the protocol for each entry.  As a comment after each entry, please list
// at least one client or test that uses the function of the listed class name.  This makes it
// easier to track what clients may be affected by removing an interface. See
// https://fxbug.dev/331409420 for more information about migrating off of the
// fuchsia.device/Controller protocol.
const std::unordered_map<std::string, std::unordered_set<std::string_view>> kControllerAllowlists({
    {"ConnectToController", {kAllowAllUses}},
    {"ConnectToDeviceFidl", {kAllowAllUses}},
    {"Bind", {kAllowAllUses}},
    {"Rebind", {kAllowAllUses}},
    {"UnbindChildren", {kAllowAllUses}},
    {"ScheduleUnbind", {kAllowAllUses}},
    {"GetTopologicalPath", {kAllowAllUses}},
});

}  // namespace

void ControllerAllowlistPassthrough::CheckAllowlist(const std::string &function_name) {
  auto allowlist = kControllerAllowlists.find(function_name);
  ZX_ASSERT_MSG(allowlist != kControllerAllowlists.end(), "Function %s was not on the allowlist",
                function_name.c_str());
  // If the kAllowAllUses string is present, just skip the check for that function.
  if (allowlist->second.find(kAllowAllUses) != allowlist->second.end()) {
    return;
  }
  ZX_ASSERT_MSG(allowlist->second.find(class_name_) != allowlist->second.end(),
                "\nUndeclared DEVFS_USAGE detected: %s is using %s.\n"
                "This error is thrown when a fuchsia.device/Controller client\n"
                "attempts to access a device and function not registered at\n"
                "src/devices/bin/driver_manager/controller_allowlist_passthrough.cc.\n"
                "If you need to add to the allowlist, add a bug as a child under "
                "https://fxbug.dev/331409420.\n",
                function_name.c_str(), class_name_.c_str());
}

void ControllerAllowlistPassthrough::ConnectToDeviceFidl(
    ConnectToDeviceFidlRequestView request, ConnectToDeviceFidlCompleter::Sync &completer) {
  CheckAllowlist("ConnectToDeviceFidl");
  if (compat_client_.is_valid()) {
    fidl::OneWayStatus status = compat_client_->ConnectToDeviceFidl(std::move(request->server));
    if (!status.ok()) {
      LOGF(ERROR, "Failed to forward ConnectToDeviceFidl call for %s.", class_name_.c_str());
    }
  } else {
    std::shared_ptr locked_node = node_.lock();
    if (!locked_node) {
      LOGF(ERROR, "Node was freed before it was used for %s.", class_name_.c_str());
      return;
    }
    locked_node->ConnectToDeviceFidl(request, completer);
  }
}

void ControllerAllowlistPassthrough::ConnectToController(
    ConnectToControllerRequestView request, ConnectToControllerCompleter::Sync &completer) {
  CheckAllowlist("ConnectToController");
  dev_controller_bindings_.AddBinding(dispatcher_, std::move(request->server), this,
                                      fidl::kIgnoreBindingClosure);
}

void ControllerAllowlistPassthrough::Bind(BindRequestView request, BindCompleter::Sync &completer) {
  CheckAllowlist("Bind");
  if (compat_client_.is_valid()) {
    compat_client_->Bind(request->driver)
        .ThenExactlyOnce([completer = completer.ToAsync()](auto &result) mutable {
          if (!result.ok()) {
            completer.ReplyError(result.status());
            return;
          }
          completer.Reply(result.value());
        });
  } else {
    std::shared_ptr locked_node = node_.lock();
    if (!locked_node) {
      LOGF(ERROR, "Node was freed before it was used for %s.", class_name_.c_str());
      return;
    }
    locked_node->Bind(request, completer);
  }
}
void ControllerAllowlistPassthrough::Rebind(RebindRequestView request,
                                            RebindCompleter::Sync &completer) {
  CheckAllowlist("Rebind");
  if (compat_client_.is_valid()) {
    compat_client_->Rebind(request->driver)
        .ThenExactlyOnce(
            [completer = completer.ToAsync()](
                fidl::WireUnownedResult<fuchsia_device::Controller::Rebind> &result) mutable {
              if (!result.ok()) {
                completer.ReplyError(result.status());
                return;
              }
              completer.Reply(result.value());
            });
  } else {
    std::shared_ptr locked_node = node_.lock();
    if (!locked_node) {
      LOGF(ERROR, "Node was freed before it was used for %s.", class_name_.c_str());
      return;
    }
    locked_node->Rebind(request, completer);
  }
}
void ControllerAllowlistPassthrough::UnbindChildren(UnbindChildrenCompleter::Sync &completer) {
  CheckAllowlist("UnbindChildren");
  if (compat_client_.is_valid()) {
    compat_client_->UnbindChildren().ThenExactlyOnce(
        [completer = completer.ToAsync()](auto &result) mutable {
          if (!result.ok()) {
            completer.ReplyError(result.status());
            return;
          }
          completer.Reply(result.value());
        });
  } else {
    std::shared_ptr locked_node = node_.lock();
    if (!locked_node) {
      LOGF(ERROR, "Node was freed before it was used for %s.", class_name_.c_str());
      return;
    }
    locked_node->UnbindChildren(completer);
  }
}
void ControllerAllowlistPassthrough::ScheduleUnbind(ScheduleUnbindCompleter::Sync &completer) {
  CheckAllowlist("ScheduleUnbind");
  if (compat_client_.is_valid()) {
    compat_client_->ScheduleUnbind().ThenExactlyOnce(
        [completer = completer.ToAsync()](auto &result) mutable {
          if (!result.ok()) {
            completer.ReplyError(result.status());
            return;
          }
          completer.Reply(result.value());
        });
  } else {
    std::shared_ptr locked_node = node_.lock();
    if (!locked_node) {
      LOGF(ERROR, "Node was freed before it was used for %s.", class_name_.c_str());
      return;
    }
    locked_node->ScheduleUnbind(completer);
  }
}
void ControllerAllowlistPassthrough::GetTopologicalPath(
    GetTopologicalPathCompleter::Sync &completer) {
  CheckAllowlist("GetTopologicalPath");
  if (compat_client_.is_valid()) {
    compat_client_->GetTopologicalPath().ThenExactlyOnce(
        [completer = completer.ToAsync()](auto &result) mutable {
          completer.Reply(*result.Unwrap());
        });
  } else {
    std::shared_ptr locked_node = node_.lock();
    if (!locked_node) {
      LOGF(ERROR, "Node was freed before it was used for %s.", class_name_.c_str());
      return;
    }
    locked_node->GetTopologicalPath(completer);
  }
}

zx_status_t ControllerAllowlistPassthrough::Connect(
    fidl::ServerEnd<fuchsia_device::Controller> server_end) {
  dev_controller_bindings_.AddBinding(dispatcher_, std::move(server_end), this,
                                      fidl::kIgnoreBindingClosure);
  return ZX_OK;
}

std::unique_ptr<ControllerAllowlistPassthrough> ControllerAllowlistPassthrough::Create(
    std::optional<fidl::ClientEnd<fuchsia_device_fs::Connector>> controller_connector,
    std::weak_ptr<fidl::WireServer<fuchsia_device::Controller>> node,
    async_dispatcher_t *dispatcher, const std::string &class_name) {
  std::unique_ptr<ControllerAllowlistPassthrough> ret(
      new ControllerAllowlistPassthrough(std::move(node), dispatcher, class_name));
  if (!controller_connector.has_value()) {
    return ret;
  }
  auto [client_end, server_end] = fidl::Endpoints<fuchsia_device::Controller>::Create();

  zx_status_t status =
      fidl::WireCall(controller_connector.value())->Connect(server_end.TakeChannel()).status();
  if (status != ZX_OK) {
    return ret;
  }
  ret->compat_client_.Bind(std::move(client_end), dispatcher);
  return ret;
}
