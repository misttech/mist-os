// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "misc/drivers/mistos/driver.h"

#include <trace.h>

#include <fbl/alloc_checker.h>
#include <ktl/unique_ptr.h>

#define LOCAL_TRACE 0

namespace mistos {

DriverBase::DriverBase(std::string_view name) : name_(name) {}

DriverBase::~DriverBase() {}

Driver::Driver(device_t device) : DriverBase(device.name), device_(device, this) {}

Driver::~Driver() {}

zx_status_t Driver::Start() { return StartDriver(); }

zx_status_t Driver::StartDriver() {
  if (record_->ops->init != nullptr) {
    // If provided, run init.
    zx_status_t status = record_->ops->init(&context_);
    if (status != ZX_OK) {
      LTRACEF("Failed to load driver '%.*s', 'init' failed: %d\n",
              static_cast<int>(driver_name_.size()), driver_name_.data(), status);
      return status;
    }
  }

  if (record_->ops->bind != nullptr) {
    // If provided, run bind and return.
    zx_status_t status = record_->ops->bind(context_, device_.ZxDevice());
    if (status != ZX_OK) {
      LTRACEF("Failed to load driver '%.*s', 'bind' failed: %d\n",
              static_cast<int>(driver_name_.size()), driver_name_.data(), status);
      return status;
    }
  } else {
    // Else, run create and return.
    zx_status_t status = record_->ops->create(context_, device_.ZxDevice(), "proxy");
    if (status != ZX_OK) {
      LTRACEF("Failed to load driver '%.*s', 'create' failed: %d\n",
              static_cast<int>(driver_name_.size()), driver_name_.data(), status);
      return status;
    }
  }
  if (!device_.HasChildren()) {
    LTRACEF("Driver '%.*s' did not add a child device\n", static_cast<int>(driver_name_.size()),
            driver_name_.data());
    return ZX_ERR_BAD_STATE;
  }

  return ZX_OK;
}

zx_status_t Driver::LoadDriver(const zircon_driver_ldr_t* loader) {
  driver_name_ = loader->note.payload.name;
  LTRACEF("Loaded driver %.*s\n", static_cast<int>(driver_name_.size()), driver_name_.data());

  record_ = static_cast<zx_driver_rec_t*>(loader->rec);

  if (record_->ops == nullptr) {
    LTRACEF("Failed to load driver '%s', missing driver ops\n", loader->note.payload.name);
    return ZX_ERR_BAD_STATE;
  }
  if (record_->ops->version != DRIVER_OPS_VERSION) {
    LTRACEF("Failed to load driver '%s', incorrect driver version\n", loader->note.payload.name);
    return ZX_ERR_WRONG_TYPE;
  }
  if (record_->ops->bind == nullptr && record_->ops->create == nullptr) {
    LTRACEF("Failed to load driver '%s', missing '%s'\n", loader->note.payload.name,
            (record_->ops->bind == nullptr ? "bind" : "create"));
    return ZX_ERR_BAD_STATE;
  }
  if (record_->ops->bind != nullptr && record_->ops->create != nullptr) {
    LTRACEF("Failed to load driver '%s', both 'bind' and 'create' are defined\n",
            loader->note.payload.name);
    return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

Driver* CreateDriver(const zircon_driver_ldr_t* driver_loader) {
  auto compat_device = &kDefaultDevice;

  fbl::AllocChecker ac;
  auto driver = ktl::make_unique<Driver>(&ac, *compat_device);
  if (!ac.check()) {
    return nullptr;
  }

  if (ZX_OK != driver->LoadDriver(driver_loader)) {
    return nullptr;
  }

  if (ZX_OK != driver->Start()) {
    return nullptr;
  }

  return driver.release();
}

}  // namespace mistos
