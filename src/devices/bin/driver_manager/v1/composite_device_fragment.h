// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_COMPOSITE_DEVICE_FRAGMENT_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_COMPOSITE_DEVICE_FRAGMENT_H_

#include <fidl/fuchsia.driver.development/cpp/wire.h>
#include <lib/ddk/binding.h>

#include <fbl/array.h>

#include "src/devices/bin/driver_manager/driver.h"
#include "src/devices/bin/driver_manager/metadata.h"

class CompositeDevice;
class Device;
class DriverHost;

// Tags used for container membership identification.
namespace internal {
struct CdfListTag {};
struct CdfDeviceListTag {};
}  // namespace internal

// This class represents a fragment of a composite device.
// It holds information like the bind instructions, and if a device has been
// bound to the fragment.
// It also helps create proxies for the composite.
class CompositeDeviceFragment
    : public fbl::ContainableBaseClasses<
          fbl::TaggedDoublyLinkedListable<std::unique_ptr<CompositeDeviceFragment>,
                                          internal::CdfListTag,
                                          fbl::NodeOptions::AllowMultiContainerUptr>,
          fbl::TaggedDoublyLinkedListable<CompositeDeviceFragment*, internal::CdfDeviceListTag>> {
 public:
  using DeviceListTag = internal::CdfDeviceListTag;

  CompositeDeviceFragment(CompositeDevice* composite, std::string name, uint32_t index,
                          fbl::Array<const zx_bind_inst_t> bind_rules);

  CompositeDeviceFragment(CompositeDeviceFragment&&) = delete;
  CompositeDeviceFragment& operator=(CompositeDeviceFragment&&) = delete;

  CompositeDeviceFragment(const CompositeDeviceFragment&) = delete;
  CompositeDeviceFragment& operator=(const CompositeDeviceFragment&) = delete;

  ~CompositeDeviceFragment();

  // Attempt to match this fragment against |dev|.  Returns true if the match
  // was successful.
  bool TryMatch(const fbl::RefPtr<Device>& dev) const;

  // Bind this fragment to the given device.
  zx_status_t Bind(const fbl::RefPtr<Device>& dev);

  // Unbind this fragment.
  void Unbind();

  bool IsBound() const { return bound_device_ != nullptr; }

  // This is true if this fragment is ready for assembly. That means that it
  // is bound and its fragment driver (or fidl proxy) is ready.
  bool IsReady() const;

  // Create a proxy for this fragment in the specific driver host.
  // This may create a Banjo or FIDL proxy depending on the device bound to this
  // fragment.
  zx_status_t CreateProxy(const fbl::RefPtr<DriverHost> driver_host);

  fuchsia_driver_development::wire::LegacyCompositeFragmentInfo GetCompositeFragmentInfo(
      fidl::AnyArena& arena) const;

  std::string_view name() const { return name_; }
  uint32_t index() const { return index_; }
  CompositeDevice* composite() const { return composite_; }

  // This is the device that the fragment matches with.
  const fbl::RefPtr<Device>& bound_device() const { return bound_device_; }

  // This is the device created by the fragment driver (fragment.cm).
  // It is the same driver host as `bound_device()`.
  // This does only exists if `bound_device()` needs banjo proxying.
  const fbl::RefPtr<Device>& fragment_device() const { return fragment_device_; }

  // This is the proxy device that lives in the driver host of the composite
  // device.
  // If `bound_device()` does banjo proxying this is a device from fragment.proxy.cm.
  // If `bound_device()` does FIDL proxying this is a fidl proxy.
  // If `bound_device()` is the primary parent and the composite is colocated
  // this the same as `bound_device()`.
  const fbl::RefPtr<Device>& proxy_device() const { return proxy_device_; }

  // Registers (or unregisters) the fragment device (i.e. an instance of the
  // "fragment" driver) that bound to bound_device().
  void set_fragment_device(fbl::RefPtr<Device> device) { fragment_device_ = std::move(device); }

  void set_proxy_device(fbl::RefPtr<Device> device) { proxy_device_ = std::move(device); }

  bool uses_fragment_driver() const { return uses_fragment_driver_; }

 private:
  // The CompositeDevice that this is a part of.
  CompositeDevice* const composite_;

  // The name of this fragment within its CompositeDevice.
  const std::string name_;

  // The index of this fragment within its CompositeDevice.
  const uint32_t index_;

  // Bind rules for the fragment.
  const fbl::Array<const zx_bind_inst_t> bind_rules_;

  // This is the device that the composite fragment has matched to.
  // If this is null then this fragment does not have a device yet.
  //
  // Note that if this device needs to be Banjo proxied, then we will create
  // `fragment_device_` to do the proxying.
  fbl::RefPtr<Device> bound_device_ = nullptr;

  // This is the device created by the fragment driver (fragment.cm) that
  // was bound to `bound_device_`.
  //
  // Note that this is fragment.cm, not fragment.proxy.cm, this device lives
  // in the same driver host as `bound_device_`.
  //
  // This only exists if `bound_device_` needs to be Banjo proxied.
  fbl::RefPtr<Device> fragment_device_ = nullptr;

  // This is the device that lives in the driver_host of the final composite device.
  // It may be a Banjo proxied device, it may be a FIDL proxied device, or it may
  // be `bound_device_` if the composite is colocated.
  fbl::RefPtr<Device> proxy_device_ = nullptr;

  bool uses_fragment_driver_ = false;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_COMPOSITE_DEVICE_FRAGMENT_H_
