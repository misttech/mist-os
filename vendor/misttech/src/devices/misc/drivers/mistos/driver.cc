// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "misc/drivers/mistos/driver.h"

#include <lib/fit/function.h>
#include <trace.h>

#include <fbl/alloc_checker.h>
#include <ktl/unique_ptr.h>

#define LOCAL_TRACE 2
// #define VERBOSE_DRIVER_LOAD 1

namespace mistos {

namespace {

#if 0
// CompatDriverServer::CreateDriver
Driver* CreateDriver(const zircon_driver_note_t* note, zx_driver_rec_t* rec,
                     ktl::unique_ptr<const zx_bind_inst_t[]> binding) {

  auto compat_device = &kDefaultDevice;

  const zx_protocol_device_t* ops = nullptr;

  fbl::AllocChecker ac;
  auto driver = ktl::make_unique<Driver>(&ac, *compat_device, ops);
  if (!ac.check()) {
    return nullptr;
  }

  if (zx::result result = driver->LoadDriver(note->payload.name, rec, std::move(binding));
      result.is_error()) {
    LTRACEF("Failed to load driver: %s\n", result.status_string());
    return nullptr;
  }

  if (zx::result result = driver->Start(); result.is_error()) {
    LTRACEF("Failed to start driver: %s\n", result.status_string());
    return nullptr;
  }

  return driver.release();
  return nullptr;
}
#endif


}  // namespace

DriverBase::DriverBase(std::string_view name) : name_(name) {}

DriverBase::~DriverBase() {}

Driver::Driver(device_t device, const zx_protocol_device_t* ops)
    : DriverBase(device.name), device_(device, ops, this, std::nullopt) {
  // Give the parent device the correct node.
  // device_.Bind({std::move(node()), dispatcher()});
  // Call this so the parent device is in the post-init state.
  // device_.InitReply(ZX_OK);
  // ZX_ASSERT(url().has_value());
}

Driver::~Driver() {
  // if (ShouldCallRelease()) {
  //   record_->ops->release(context_);
  // }
  // dlclose(library_);
  // {
  //   std::lock_guard guard(kGlobalLoggerListLock);
  //   global_logger_list.RemoveLogger(driver_path(), inner_logger_, node_name());
  // }
}

zx::result<> Driver::Start() {
  if (zx::result result = LoadDriver(); result.is_error()) {
    LTRACEF("Failed to load driver: %d\n", result.error_value());
    return result.take_error();
  }
  return StartDriver();
}

zx::result<> Driver::StartDriver() {
  if (record_->ops->init != nullptr) {
    // If provided, run init.
    zx_status_t status = record_->ops->init(&context_);
    if (status != ZX_OK) {
      LTRACEF("Failed to load driver '%.*s', 'init' failed: %d\n",
              static_cast<int>(driver_name_.size()), driver_name_.data(), status);
      return zx::error(status);
    }
  }

  if (record_->ops->bind != nullptr) {
    // If provided, run bind and return.
    zx_status_t status = record_->ops->bind(context_, device_.ZxDevice());
    if (status != ZX_OK) {
      LTRACEF("Failed to load driver '%.*s', 'bind' failed: %d\n",
              static_cast<int>(driver_name_.size()), driver_name_.data(), status);
      return zx::error(status);
    }
  } else {
    // Else, run create and return.
    zx_status_t status = record_->ops->create(context_, device_.ZxDevice(), "proxy");
    if (status != ZX_OK) {
      LTRACEF("Failed to load driver '%.*s', 'create' failed: %d\n",
              static_cast<int>(driver_name_.size()), driver_name_.data(), status);
      return zx::error(status);
    }
  }
  if (!device_.HasChildren()) {
    LTRACEF("Driver '%.*s' did not add a child device\n", static_cast<int>(driver_name_.size()),
            driver_name_.data());
    return zx::error(ZX_ERR_BAD_STATE);
  }

  return zx::ok();
}

zx::result<> Driver::LoadDriver() {
  LTRACEF("Loaded driver %.*s\n", static_cast<int>(driver_name_.size()), driver_name_.data());
  ZX_ASSERT(record_ != nullptr);

  if (record_->ops == nullptr) {
    LTRACEF("Failed to load driver '%.*s', missing driver ops\n",
            static_cast<int>(driver_name_.size()), driver_name_.data());
    return zx::error(ZX_ERR_BAD_STATE);
  }
  if (record_->ops->version != DRIVER_OPS_VERSION) {
    LTRACEF("Failed to load driver '%.*s', incorrect driver version\n",
            static_cast<int>(driver_name_.size()), driver_name_.data());
    return zx::error(ZX_ERR_WRONG_TYPE);
  }
  if (record_->ops->bind == nullptr && record_->ops->create == nullptr) {
    LTRACEF("Failed to load driver '%.*s', missing '%s'\n", static_cast<int>(driver_name_.size()),
            driver_name_.data(), (record_->ops->bind == nullptr ? "bind" : "create"));
    return zx::error(ZX_ERR_BAD_STATE);
  }
  if (record_->ops->bind != nullptr && record_->ops->create != nullptr) {
    LTRACEF("Failed to load driver '%.*s', both 'bind' and 'create' are defined\n",
            static_cast<int>(driver_name_.size()), driver_name_.data());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok();
}

zx_status_t Driver::AddDevice(Device* parent, device_add_args_t* args, zx_device_t** out) {
  zx_device_t* child;
  zx_status_t status = parent->Add(args, &child);
  if (status != ZX_OK) {
    LTRACEF("Failed to add device %s: %s\n", args->name, zx_status_get_string(status));
    return status;
  }
  if (out) {
    *out = child;
  }
  return ZX_OK;
}

zx_status_t Driver::GetProtocol(uint32_t proto_id, void* out) { return ZX_ERR_NOT_SUPPORTED; }

zx_status_t Driver::GetFragmentProtocol(const char* fragment, uint32_t proto_id, void* out) {
  return ZX_ERR_NOT_SUPPORTED;
}

void Driver::CompleteStart(zx::result<> result) {}

}  // namespace mistos
