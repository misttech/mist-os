// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_DDKTL_INCLUDE_DDKTL_SERVICE_H_
#define SRC_LIB_DDKTL_INCLUDE_DDKTL_SERVICE_H_

#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/device.h>

namespace ddk {

// AddService allows a DFV1 driver to advertise a FIDL service.
//
// The intended use is for DFv1 drivers to use this to advertise services to non-drivers.
// It is only really supported in the compat shim, where it adds the service to the outgoing
// directory that the compat shim maintains.
//
// |device| is the device whose outgoing directory will contain the service. The device that
// advertises a service must declare the service capability in its cml file.  Note that this
// function will only work if DdkAdd has already been called on the device, and the device does not
// need to add a child to advertise the service, unlike devfs.
// |handler| is the handler for the service.
// |instance_name| is the name of the instance to advertise.  If this service is going to be
// aggregated, the aggregated instance name will be randomized.
template <typename Service, typename = std::enable_if_t<fidl::IsServiceV<Service>>>
zx::result<> AddService(
    zx_device_t *device, component::ServiceInstanceHandler handler,
    const std::string &instance_name = component::OutgoingDirectory::kDefaultServiceInstance) {
  static_assert(fidl::IsService<Service>(), "Type of |Service| must be FIDL service");
  ZX_ASSERT_MSG(device, "AddService called on uninitialized device. You must to call DdkAdd first");

  auto handlers = handler.TakeMemberHandlers();
  if (handlers.empty()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  std::string service_name = Service::Name;

  for (auto &[member_name, member_handler] : handlers) {
    zx_status_t status = device_register_service_member(
        device, static_cast<void *>(&member_handler), service_name.c_str(), instance_name.c_str(),
        member_name.c_str());
    if (status != ZX_OK) {
      return zx::make_result(status);
    }
  }
  return zx::ok();
}

}  // namespace ddk

#endif  // SRC_LIB_DDKTL_INCLUDE_DDKTL_SERVICE_H_
