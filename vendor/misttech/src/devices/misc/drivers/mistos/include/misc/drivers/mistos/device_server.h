// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DEVICE_SERVER_H_
#define VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DEVICE_SERVER_H_

#include <lib/fit/function.h>
#include <lib/mistos/util/allocator.h>
#include <lib/zx/result.h>

#include <map>

namespace mistos {

using BanjoProtoId = uint32_t;

class DeviceServer {
 public:
  struct GenericProtocol {
    const void* ops;
    void* ctx;
  };

  using SpecificGetBanjoProtoCb = fit::function<GenericProtocol()>;
  using GenericGetBanjoProtoCb = fit::function<zx::result<GenericProtocol>(BanjoProtoId)>;

  struct BanjoConfig {
    BanjoProtoId default_proto_id = 0;
    GenericGetBanjoProtoCb generic_callback = nullptr;
    std::map<BanjoProtoId, SpecificGetBanjoProtoCb, std::less<>,
             util::Allocator<std::pair<const BanjoProtoId, SpecificGetBanjoProtoCb>>>
        callbacks = {};
  };

  DeviceServer() = default;

  // Functions to implement the DFv1 device API.
  // zx_status_t AddMetadata(MetadataKey type, const void* data, size_t size);
  // zx_status_t GetMetadata(MetadataKey type, void* buf, size_t buflen, size_t* actual);
  // zx_status_t GetMetadataSize(MetadataKey type, size_t* out_size);
  zx_status_t GetProtocol(BanjoProtoId proto_id, GenericProtocol* out) const;

  // std::string_view name() const { return name_; }
  BanjoProtoId proto_id() const {
    return banjo_config_.has_value() ? banjo_config_->default_proto_id : 0;
  }
  bool has_banjo_config() const { return banjo_config_.has_value(); }

  void Initialize(std::optional<BanjoConfig> banjo_config);

 private:
  // std::string_view name_;
  //  MetadataMap metadata_;
  //  std::optional<ServiceOffersV1> service_offers_;
  std::optional<BanjoConfig> banjo_config_;

  // fidl::ServerBindingGroup<fuchsia_driver_compat::Device> bindings_;

  // This callback is called when the class is destructed and it will stop serving the protocol.
  // fit::deferred_callback stop_serving_;
};

}  // namespace mistos

#endif  // VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_DEVICE_SERVER_H_
