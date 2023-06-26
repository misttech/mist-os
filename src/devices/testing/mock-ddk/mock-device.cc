// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/testing/mock-ddk/mock-device.h"

#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/protocol.h>

#include <algorithm>
#include <latch>

namespace {
std::mutex g_mutex;
std::weak_ptr<fdf_testing::DriverRuntime> g_runtime __TA_GUARDED(g_mutex);
}  // namespace

namespace mock_ddk {

std::shared_ptr<fdf_testing::DriverRuntime> GetDriverRuntime() {
  std::lock_guard guard(g_mutex);
  std::shared_ptr shared = g_runtime.lock();
  if (shared) {
    return shared;
  }

  shared = std::make_shared<fdf_testing::DriverRuntime>();
  g_runtime = shared;
  return shared;
}

}  // namespace mock_ddk

MockDevice::MockDevice(device_add_args_t* args, MockDevice* parent)
    : parent_(parent),
      ops_(args->ops),
      ctx_(args->ctx),
      name_(args->name),
      inspect_(zx::vmo{args->inspect_vmo}),
      outgoing_(zx::channel{args->outgoing_dir_channel}),
      driver_runtime_(parent_->driver_runtime_) {
  if (args->proto_id && args->proto_ops) {
    AddProtocol(args->proto_id, args->proto_ops, ctx_);
  }
  if (args->props) {
    props_.insert(props_.begin(), args->props, args->props + args->prop_count);
  }
  if (args->str_props) {
    str_props_.insert(str_props_.begin(), args->str_props, args->str_props + args->str_prop_count);
  }
  if (args->metadata_list && args->metadata_count > 0) {
    for (size_t i = 0; i < args->metadata_count; ++i) {
      SetMetadata(args->metadata_list[i].type, args->metadata_list[i].data,
                  args->metadata_list[i].length);
    }
  }
}

// Static member function.
zx_status_t MockDevice::Create(device_add_args_t* args, MockDevice* parent, MockDevice** out_dev) {
  // We only check the minimum requirements to make sure the mock does not crash:
  if (!parent || !args || !args->name) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Using `new` to access a non-public constructor.
  auto new_device = std::shared_ptr<MockDevice>(new MockDevice(args, parent));
  if (out_dev) {
    *out_dev = new_device.get();
  }
  parent->children_.emplace_back(std::move(new_device));
  // PropagateMetadata to last child:
  for (const auto& [key, value] : parent->metadata_) {
    parent->children().back()->metadata_[key] = value;
  }
  parent->children().back()->PropagateMetadata();
  return ZX_OK;
}

MockDevice* MockDevice::GetLatestChild() {
  if (child_count()) {
    return children_.back().get();
  }
  return nullptr;
}

// Templates that dispatch the protocol operations if they were set.
// If they were not set, the fallback parameter is returned to the caller
// (usually ZX_ERR_NOT_SUPPORTED)
template <typename RetType, typename... ArgTypes>
RetType Dispatch(void* ctx, RetType (*op)(void* ctx, ArgTypes...), RetType fallback,
                 ArgTypes... args) {
  if (!op) {
    return fallback;
  }

  return (*op)(ctx, args...);
}

template <typename... ArgTypes>
void Dispatch(void* ctx, void (*op)(void* ctx, ArgTypes...), ArgTypes... args) {
  if (!op) {
    return;
  }

  (*op)(ctx, args...);
}

void MockDevice::InitOp() { Dispatch(ctx_, ops_->init); }

void MockDevice::UnbindOp() { Dispatch(ctx_, ops_->unbind); }

void MockDevice::ReleaseOp() {
  if (!IsRootParent()) {
    while (!children_.empty()) {
      children_.front()->ReleaseOp();
    }
    parent_->ChildPreReleaseOp(ctx_);
    parent_->RecordChildPreRelease(ZX_OK);
  }

  Dispatch(ctx_, ops_->release);

  // Delete instance from parent's children_ accounting.
  for (auto it = parent_->children_.begin(); it != parent_->children_.end(); ++it) {
    if (it->get() == this) {
      parent_->children_.erase(it);
      return;
    }
  }
}

MockDevice::~MockDevice() {
  while (!children_.empty()) {
    children_.front()->ReleaseOp();
  }
}

void MockDevice::SuspendNewOp(uint8_t requested_state, bool enable_wake, uint8_t suspend_reason) {
  Dispatch(ctx_, ops_->suspend, requested_state, enable_wake, suspend_reason);
}

zx_status_t MockDevice::SetPerformanceStateOp(uint32_t requested_state, uint32_t* out_state) {
  return Dispatch(ctx_, ops_->set_performance_state, ZX_ERR_NOT_SUPPORTED, requested_state,
                  out_state);
}

zx_status_t MockDevice::ConfigureAutoSuspendOp(bool enable, uint8_t requested_state) {
  return Dispatch(ctx_, ops_->configure_auto_suspend, ZX_ERR_NOT_SUPPORTED, enable,
                  requested_state);
}

void MockDevice::ResumeNewOp(uint32_t requested_state) {
  Dispatch(ctx_, ops_->resume, requested_state);
}

bool MockDevice::MessageOp(fidl::IncomingHeaderAndMessage msg, device_fidl_txn_t txn) {
  if (ops_->message) {
    Dispatch(ctx_, ops_->message, std::move(msg).ReleaseToEncodedCMessage(), txn);
    return true;
  }

  return false;
}

void MockDevice::ChildPreReleaseOp(void* child_ctx) {
  Dispatch(ctx_, ops_->child_pre_release, child_ctx);
}

void MockDevice::SetMetadata(uint32_t type, const void* data, size_t data_length) {
  std::vector<uint8_t> owned(data_length);
  memcpy(owned.data(), data, data_length);
  metadata_[type] = std::move(owned);
  PropagateMetadata();
}

void MockDevice::SetVariable(const char* name, const char* data) {
  if (data) {
    std::string owned(data);
    variables_[name] = std::move(owned);
  } else {
    auto itr = variables_.find(name);
    if (itr != variables_.end()) {
      variables_.erase(itr);
    }
  }
}

zx_status_t MockDevice::GetMetadata(uint32_t type, void* buf, size_t buflen, size_t* actual) {
  auto itr = metadata_.find(type);
  if (itr != metadata_.end()) {
    auto& metadata = itr->second;
    *actual = metadata.size();
    if (buflen < metadata.size()) {
      return ZX_ERR_BUFFER_TOO_SMALL;
    }

    memcpy(buf, metadata.data(), metadata.size());
    return ZX_OK;
  }
  return ZX_ERR_NOT_FOUND;
}

zx_status_t MockDevice::GetMetadataSize(uint32_t type, size_t* out_size) {
  auto itr = metadata_.find(type);
  if (itr != metadata_.end()) {
    auto metadata = itr->second;
    *out_size = metadata.size();
    return ZX_OK;
  }
  return ZX_ERR_NOT_FOUND;
}

zx_status_t MockDevice::GetVariable(const char* name, char* out, size_t size, size_t* actual) {
  auto itr = variables_.find(name);
  if (itr == variables_.end()) {
    return ZX_ERR_NOT_FOUND;
  }

  auto& variable = itr->second;
  if (actual) {
    *actual = variable.size();
  }
  if (size < variable.size()) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }

  strncpy(out, variable.data(), size);
  return ZX_OK;
}

void MockDevice::PropagateMetadata() {
  for (auto& child : children_) {
    for (const auto& [key, value] : metadata_) {
      child->metadata_[key] = value;
    }
    child->PropagateMetadata();
  }
}

void MockDevice::AddProtocol(uint32_t id, const void* ops, void* ctx, const char* fragment_name) {
  protocols_[fragment_name].push_back(mock_ddk::ProtocolEntry{id, {ops, ctx}});
}

void MockDevice::AddFidlService(const char* service_name, fidl::ClientEnd<fuchsia_io::Directory> ns,
                                const char* name) {
  fidl_services_.try_emplace(
      name, std::unordered_map<std::string, fidl::ClientEnd<fuchsia_io::Directory>>());
  fidl_services_[name][service_name] = std::move(ns);
}

void MockDevice::SetFirmware(std::vector<uint8_t> firmware, std::string_view path) {
  firmware_[path] = std::move(firmware);
}

void MockDevice::SetFirmware(std::string firmware, std::string_view path) {
  std::vector<uint8_t> vec(firmware.begin(), firmware.end());
  SetFirmware(vec, path);
}

zx_status_t MockDevice::LoadFirmware(std::string_view path, zx_handle_t* fw, size_t* size) {
  auto firmware = firmware_.find(path);
  // If a match is not found to 'path', check if there is a firmware that was loaded with
  // path == nullptr:
  if (firmware == firmware_.end()) {
    firmware = firmware_.find("");
  }
  if (firmware == firmware_.end()) {
    return ZX_ERR_NOT_FOUND;
  }
  zx_status_t status = ZX_OK;
  zx_handle_t vmo = ZX_HANDLE_INVALID;
  if ((status = zx_vmo_create(firmware->second.size(), 0, &vmo)) != ZX_OK) {
    return status;
  }
  if ((status = zx_vmo_write(vmo, firmware->second.data(), 0, firmware->second.size())) != ZX_OK) {
    return status;
  }

  *fw = vmo;
  *size = firmware->second.size();
  return ZX_OK;
}

zx_status_t MockDevice::GetProtocol(uint32_t proto_id, void* protocol,
                                    const char* fragment_name) const {
  // Check if there are protocols for the fragment/device:
  auto protocol_set = protocols_.find(fragment_name);
  if (protocol_set == protocols_.end()) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  auto out = reinterpret_cast<mock_ddk::Protocol*>(protocol);
  // First we check if the user has added protocols:
  for (const auto& proto : protocol_set->second) {
    if (proto_id == proto.id) {
      out->ops = proto.proto.ops;
      out->ctx = proto.proto.ctx;
      return ZX_OK;
    }
  }
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t MockDevice::ConnectToFidlProtocol(const char* service_name, const char* protocol_name,
                                              zx::channel request, const char* fragment_name) {
  // Check if there are protocols for the fragment/device:
  auto service_set = fidl_services_.find(fragment_name);
  if (service_set == fidl_services_.end()) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  auto ns = service_set->second.find(service_name);
  if (ns == service_set->second.end()) {
    return ZX_ERR_NOT_FOUND;
  }
  const std::string path = std::string("svc/") + service_name + "/default/" + protocol_name;
  return component::internal::ConnectAtRaw(ns->second, std::move(request), path.c_str())
      .status_value();
}

size_t MockDevice::descendant_count() const {
  size_t count = child_count();
  for (auto& child : children_) {
    count += child->descendant_count();
  }
  return count;
}

// helper functions:
namespace {

zx_status_t ProcessDeviceRemoval(MockDevice* device, async_dispatcher_t* dispatcher) {
  if (dispatcher != nullptr) {
    std::latch done(1);
    async::PostTask(dispatcher, [&]() {
      device->UnbindOp();
      done.count_down();
    });
    done.wait();
  } else {
    device->UnbindOp();
  }
  // deleting children, so use a while loop:
  while (!device->children().empty()) {
    auto status = ProcessDeviceRemoval(device->children().back().get(), dispatcher);
    if (status != ZX_OK) {
      return status;
    }
  }
  if (device->HasUnbindOp()) {
    zx_status_t status = device->WaitUntilUnbindReplyCalled();
    if (status != ZX_OK) {
      return status;
    }
  }
  if (dispatcher != nullptr) {
    std::latch done(1);
    async::PostTask(dispatcher, [&]() {
      device->ReleaseOp();
      done.count_down();
    });
    done.wait();
  } else {
    device->ReleaseOp();
  }
  return ZX_OK;
}
}  // anonymous namespace

zx_status_t mock_ddk::ReleaseFlaggedDevices(MockDevice* device, async_dispatcher_t* dispatcher) {
  if (device->AsyncRemoveCalled()) {
    return ProcessDeviceRemoval(device, dispatcher);
  }
  // Make a vector of the child device pointers, because we might delete the child:
  std::vector<MockDevice*> children;
  std::transform(device->children().begin(), device->children().end(), std::back_inserter(children),
                 [](std::shared_ptr<MockDevice> c) -> MockDevice* { return c.get(); });
  for (auto child : children) {
    auto ret = ReleaseFlaggedDevices(child, dispatcher);
    if (ret != ZX_OK) {
      return ret;
    }
  }
  return ZX_OK;
}
