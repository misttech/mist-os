// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// #include <lib/ddk/debug.h>
// #include <lib/zx/profile.h>
#include <zircon/errors.h>

// #include <ddktl/fidl.h>

#include "misc/drivers/mistos/device.h"
#include "misc/drivers/mistos/driver.h"

extern "C" {
zx_status_t device_add_from_driver(zx_driver_t* drv, zx_device_t* parent, device_add_args_t* args,
                                   zx_device_t** out) {
  return parent->driver()->AddDevice(parent, args, out);
}

zx_status_t device_get_properties(zx_device_t* device, device_props_args_t* out_args) {
  return device->driver()->GetProperties(out_args);
}

void device_init_reply(zx_device_t* dev, zx_status_t status, const device_init_reply_args_t* args) {
  dev->InitReply(status);
}

void device_async_remove(zx_device_t* dev) {
  // dev->Remove();
}

void device_unbind_reply(zx_device_t* dev) {
  // dev->CompleteUnbind();
}

void device_suspend_reply(zx_device_t* dev, zx_status_t status, uint8_t out_state) {
  // dev->CompleteSuspend();
}

void device_resume_reply(zx_device_t* dev, zx_status_t status, uint8_t out_power_state,
                         uint32_t out_perf_state) {}

zx_status_t device_set_profile_by_role(zx_device_t* device, zx_handle_t thread, const char* role,
                                       size_t role_size) {
#if 0
  if (device == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }
  return device->driver()
      ->SetProfileByRole(zx::unowned_thread(thread), std::string_view(role, role_size))
      .status_value();
#endif
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t device_get_protocol(const zx_device_t* dev, uint32_t proto_id, void* out) {
  return dev->GetProtocol(proto_id, out);
}

zx_status_t device_get_config_vmo(zx_device_t* device, zx_handle_t* config_vmo) {
#if 0
  if (device == nullptr || config_vmo == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }
  *config_vmo = device->driver()->GetConfigVmo().release();
  return ZX_OK;
#endif
  return ZX_ERR_NOT_SUPPORTED;
}

// LibDriver Misc Interfaces

void* get_mmio_resource(zx_device_t* device) {
  // return device->driver()->GetMmioResource();
  return nullptr;
}

void* get_msi_resource(zx_device_t* device) {
  // return device->driver()->GetMsiResource();
  return nullptr;
}

void* get_power_resource(zx_device_t* device) {
  // return device->driver()->GetPowerResource();
  return nullptr;
}

void* get_iommu_resource(zx_device_t* device) {
  // return device->driver()->GetIommuResource();
  return nullptr;
}

void* get_ioport_resource(zx_device_t* device) {
  // return device->driver()->GetIoportResource();
  return nullptr;
}

void* get_irq_resource(zx_device_t* device) {
  // return device->driver()->GetIrqResource();
  return nullptr;
}

void* get_smc_resource(zx_device_t* device) {
  // return device->driver()->GetSmcResource();
  return nullptr;
}

void* get_info_resource(zx_device_t* device) {
  // return device->driver()->GetInfoResource();
  return nullptr;
}

zx_status_t load_firmware_from_driver(zx_driver_t* drv, zx_device_t* dev, const char* path,
                                      void** fw, size_t* size) {
#if 0
  auto result = dev->driver()->LoadFirmware(dev, path, size);
  if (result.is_error()) {
    return result.error_value();
  }
  *fw = result->release();
  return ZX_OK;
#endif
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t device_get_metadata(zx_device_t* dev, uint32_t type, void* buf, size_t buflen,
                                size_t* actual) {
  return dev->GetMetadata(type, buf, buflen, actual);
}

zx_status_t device_get_metadata_size(zx_device_t* dev, uint32_t type, size_t* out_size) {
  return dev->GetMetadataSize(type, out_size);
}

zx_status_t device_add_metadata(zx_device_t* dev, uint32_t type, const void* data, size_t size) {
  return dev->AddMetadata(type, data, size);
}

zx_status_t device_add_composite_spec(zx_device_t* dev, const char* name,
                                      const composite_node_spec_t* spec) {
#if 0
  return dev->AddCompositeNodeSpec(name, spec);
#endif
  return ZX_ERR_NOT_SUPPORTED;
}

#if 0
bool driver_log_severity_enabled_internal(const zx_driver_t* drv, fx_log_severity_t severity) {
  return drv->IsSeverityEnabled(static_cast<FuchsiaLogSeverity>(severity));
}

void driver_logvf_internal(const zx_driver_t* drv, fx_log_severity_t severity, const char* tag,
                           const char* file, int line, const char* msg, va_list args) {
  const_cast<zx_driver_t*>(drv)->Log(static_cast<FuchsiaLogSeverity>(severity), tag, file, line,
                                     msg, args);
}

void driver_logf_internal(const zx_driver_t* drv, fx_log_severity_t severity, const char* tag,
                          const char* file, int line, const char* msg, ...) {
  va_list args;
  va_start(args, msg);
  const_cast<zx_driver_t*>(drv)->Log(static_cast<FuchsiaLogSeverity>(severity), tag, file, line,
                                     msg, args);
  va_end(args);
}
#endif

zx_status_t device_get_fragment_protocol(zx_device_t* dev, const char* name, uint32_t proto_id,
                                         void* out) {
#if 0 
  bool has_fragment =
      std::find(dev->fragments().begin(), dev->fragments().end(), name) != dev->fragments().end();
  if (!has_fragment) {
    return ZX_ERR_NOT_FOUND;
  }

  return dev->GetFragmentProtocol(name, proto_id, out);
#endif
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t device_get_fragment_metadata(zx_device_t* dev, const char* name, uint32_t type,
                                         void* buf, size_t buflen, size_t* actual) {
#if 0
  return dev->GetMetadata(type, buf, buflen, actual);
#endif
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t device_register_service_member(zx_device_t* dev, void* handler,
                                           const char* service_name, const char* instance_name,
                                           const char* member_name) {
#if 0
  return dev->RegisterServiceMember(std::move(*reinterpret_cast<component::AnyHandler*>(handler)),
                                    service_name, instance_name, member_name);
#endif
  return ZX_ERR_NOT_SUPPORTED;
}

#if 0
zx_status_t device_connect_fidl_protocol2(zx_device_t* device, const char* service_name,
                                          const char* protocol_name, zx_handle_t request) {
  return device->ConnectFragmentFidl("default", service_name, protocol_name, zx::channel(request));
}

zx_status_t device_connect_fragment_fidl_protocol(zx_device_t* device, const char* fragment_name,
                                                  const char* service_name,
                                                  const char* protocol_name, zx_handle_t request) {
  return device->ConnectFragmentFidl(fragment_name, service_name, protocol_name,
                                     zx::channel(request));
}

zx_status_t device_connect_runtime_protocol(zx_device_t* dev, const char* service_name,
                                            const char* protocol_name, fdf_handle_t request) {
  return dev->ConnectFragmentRuntime("default", service_name, protocol_name, fdf::Channel(request));
}

zx_status_t device_connect_fragment_runtime_protocol(zx_device_t* dev, const char* fragment_name,
                                                     const char* service_name,
                                                     const char* protocol_name,
                                                     fdf_handle_t request) {
  return dev->ConnectFragmentRuntime(fragment_name, service_name, protocol_name,
                                     fdf::Channel(request));
}

zx_status_t device_connect_ns_protocol(zx_device_t* dev, const char* protocol_name,
                                       zx_handle_t request) {
  return dev->ConnectNsProtocol(protocol_name, zx::channel(request));
}
#endif
}
