// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/debug.h>
#include <lib/zx/profile.h>
#include <zircon/errors.h>

#include <ddktl/fidl.h>

#include "src/devices/misc/drivers/compat/device.h"
#include "src/devices/misc/drivers/compat/driver.h"

extern "C" {
__EXPORT zx_status_t device_add_from_driver(zx_driver_t* drv, zx_device_t* parent,
                                            device_add_args_t* args, zx_device_t** out) {
  return parent->driver()->AddDevice(parent, args, out);
}

__EXPORT void device_init_reply(zx_device_t* dev, zx_status_t status,
                                const device_init_reply_args_t* args) {
  dev->InitReply(status);
}

__EXPORT void device_async_remove(zx_device_t* dev) { dev->Remove(); }

__EXPORT void device_unbind_reply(zx_device_t* dev) { dev->CompleteUnbind(); }

__EXPORT void device_suspend_reply(zx_device_t* dev, zx_status_t status, uint8_t out_state) {
  dev->CompleteSuspend();
}

__EXPORT void device_resume_reply(zx_device_t* dev, zx_status_t status, uint8_t out_power_state,
                                  uint32_t out_perf_state) {}

__EXPORT zx_status_t device_set_profile_by_role(zx_device_t* device, zx_handle_t thread,
                                                const char* role, size_t role_size) {
  if (device == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }
  return device->driver()
      ->SetProfileByRole(zx::unowned_thread(thread), std::string_view(role, role_size))
      .status_value();
}

__EXPORT zx_status_t device_get_protocol(const zx_device_t* dev, uint32_t proto_id, void* out) {
  return dev->GetProtocol(proto_id, out);
}

// LibDriver Misc Interfaces

__EXPORT zx_handle_t get_root_resource(zx_device_t* device) {
  return device->driver()->GetRootResource();
}

__EXPORT zx_status_t load_firmware_from_driver(zx_driver_t* drv, zx_device_t* dev, const char* path,
                                               zx_handle_t* fw, size_t* size) {
  auto result = dev->driver()->LoadFirmware(dev, path, size);
  if (result.is_error()) {
    return result.error_value();
  }
  *fw = result->release();
  return ZX_OK;
}

__EXPORT zx_status_t device_get_metadata(zx_device_t* dev, uint32_t type, void* buf, size_t buflen,
                                         size_t* actual) {
  return dev->GetMetadata(type, buf, buflen, actual);
}

__EXPORT zx_status_t device_get_metadata_size(zx_device_t* dev, uint32_t type, size_t* out_size) {
  return dev->GetMetadataSize(type, out_size);
}

__EXPORT zx_status_t device_add_metadata(zx_device_t* dev, uint32_t type, const void* data,
                                         size_t size) {
  return dev->AddMetadata(type, data, size);
}

__EXPORT zx_status_t device_add_composite(zx_device_t* dev, const char* name,
                                          const composite_device_desc_t* comp_desc) {
  return dev->AddComposite(name, comp_desc);
}

__EXPORT zx_status_t device_add_composite_spec(zx_device_t* dev, const char* name,
                                               const composite_node_spec_t* spec) {
  return dev->AddCompositeNodeSpec(name, spec);
}

__EXPORT bool driver_log_severity_enabled_internal(const zx_driver_t* drv,
                                                   fx_log_severity_t severity) {
  return severity >= FX_LOG_SEVERITY_DEFAULT;
}

__EXPORT void driver_logvf_internal(const zx_driver_t* drv, fx_log_severity_t severity,
                                    const char* tag, const char* file, int line, const char* msg,
                                    va_list args) {
  const_cast<zx_driver_t*>(drv)->Log(static_cast<FuchsiaLogSeverity>(severity), tag, file, line,
                                     msg, args);
}

__EXPORT void driver_logf_internal(const zx_driver_t* drv, fx_log_severity_t severity,
                                   const char* tag, const char* file, int line, const char* msg,
                                   ...) {
  va_list args;
  va_start(args, msg);
  const_cast<zx_driver_t*>(drv)->Log(static_cast<FuchsiaLogSeverity>(severity), tag, file, line,
                                     msg, args);
  va_end(args);
}

__EXPORT uint32_t device_get_fragment_count(zx_device_t* dev) {
  return static_cast<uint32_t>(dev->fragments().size());
}

__EXPORT void device_get_fragments(zx_device_t* dev, composite_device_fragment_t* comp_list,
                                   size_t comp_count, size_t* comp_actual) {
  size_t i = 0;
  for (auto& fragment : dev->fragments()) {
    size_t size = sizeof(comp_list[0].name);
    if (fragment.size() < size) {
      size = fragment.size();
    }
    strncpy(comp_list[i].name, fragment.data(), size);
    // TODO(fxbug.dev/93678): We currently don't set the device pointer.
    comp_list[i].device = nullptr;
    i++;
  }
  *comp_actual = i;
}

__EXPORT zx_status_t device_get_fragment_protocol(zx_device_t* dev, const char* name,
                                                  uint32_t proto_id, void* out) {
  bool has_fragment =
      std::find(dev->fragments().begin(), dev->fragments().end(), name) != dev->fragments().end();
  if (!has_fragment) {
    return ZX_ERR_NOT_FOUND;
  }

  return dev->GetFragmentProtocol(name, proto_id, out);
}

__EXPORT zx_status_t device_get_fragment_metadata(zx_device_t* dev, const char* name, uint32_t type,
                                                  void* buf, size_t buflen, size_t* actual) {
  return dev->GetMetadata(type, buf, buflen, actual);
}

__EXPORT zx_status_t device_get_variable(zx_device_t* device, const char* name, char* out,
                                         size_t out_size, size_t* size_actual) {
  if (device == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!strncmp(name, compat::kDfv2Variable, sizeof(compat::kDfv2Variable))) {
    if (size_actual) {
      *size_actual = 2;
    }
    if (out_size < 2) {
      return ZX_ERR_BUFFER_TOO_SMALL;
    }
    out[0] = '1';
    out[1] = 0;
    return ZX_OK;
  }

  if (strncmp(name, "driver.", strlen("driver.")) == 0 ||
      strncmp(name, "clock.backstop", strlen("clock.backstop")) == 0) {
    auto variable = device->driver()->GetVariable(name);
    if (variable.is_error()) {
      return variable.status_value();
    }
    if (size_actual) {
      *size_actual = variable->size();
    }
    if (out_size < variable->size()) {
      return ZX_ERR_BUFFER_TOO_SMALL;
    }
    strcpy(out, variable->c_str());
    return ZX_OK;
  }
  return ZX_ERR_NOT_FOUND;
}

__EXPORT zx_status_t device_connect_fidl_protocol2(zx_device_t* device, const char* service_name,
                                                   const char* protocol_name, zx_handle_t request) {
  return device->ConnectFragmentFidl("default", service_name, protocol_name, zx::channel(request));
}

__EXPORT zx_status_t device_connect_fragment_fidl_protocol2(zx_device_t* device,
                                                            const char* fragment_name,
                                                            const char* service_name,
                                                            const char* protocol_name,
                                                            zx_handle_t request) {
  return device->ConnectFragmentFidl(fragment_name, service_name, protocol_name,
                                     zx::channel(request));
}

__EXPORT zx_status_t device_connect_runtime_protocol(zx_device_t* dev, const char* service_name,
                                                     const char* protocol_name,
                                                     fdf_handle_t request) {
  return dev->ConnectFragmentRuntime("default", service_name, protocol_name, fdf::Channel(request));
}

__EXPORT zx_status_t device_connect_fragment_runtime_protocol(zx_device_t* dev,
                                                              const char* fragment_name,
                                                              const char* service_name,
                                                              const char* protocol_name,
                                                              fdf_handle_t request) {
  return dev->ConnectFragmentRuntime(fragment_name, service_name, protocol_name,
                                     fdf::Channel(request));
}
}
