// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_COMPOSITE_DEVICE_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_COMPOSITE_DEVICE_H_

#include <fidl/fuchsia.device.manager/cpp/wire.h>
#include <fidl/fuchsia.driver.development/cpp/wire.h>

#include <fbl/array.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/string.h>

#include "src/devices/bin/driver_manager/v1/composite_device_fragment.h"
#include "src/devices/bin/driver_manager/v1/driver.h"
#include "src/devices/bin/driver_manager/v1/metadata.h"

class Coordinator;
class Device;
class DriverHost;

using StrPropertyValue = std::variant<uint32_t, std::string, bool, std::string>;

enum StrPropValueType { Integer = 0, String = 1, Bool = 2, Enum = 3 };

struct StrProperty {
  std::string key;
  StrPropertyValue value;
};

// Contains the information to create a CompositeDevice from a spec.
struct CompositeNodeSpecInfo {
  // The name of the spec.
  std::string spec_name;
  MatchedDriverInfo driver;

  // The name of the composite node.
  std::string composite_name;

  uint32_t primary_index;
  std::vector<std::string> parent_names;
};

// A device composed of other devices.
class CompositeDevice : public fbl::DoublyLinkedListable<std::unique_ptr<CompositeDevice>> {
 public:
  using FragmentList =
      fbl::TaggedDoublyLinkedList<std::unique_ptr<CompositeDeviceFragment>, internal::CdfListTag>;

  // Only public because of make_unique.  You probably want Create().
  CompositeDevice(fbl::String name, fbl::Array<const zx_device_prop_t> properties,
                  fbl::Array<const StrProperty> str_properties, uint32_t fragments_count,
                  uint32_t primary_fragment_index, std::optional<bool> legacy_colocate_flag,
                  fbl::Array<std::unique_ptr<Metadata>> metadata, Coordinator& coordinator,
                  bool from_driver_index);

  CompositeDevice(CompositeDevice&&) = delete;
  CompositeDevice& operator=(CompositeDevice&&) = delete;

  CompositeDevice(const CompositeDevice&) = delete;
  CompositeDevice& operator=(const CompositeDevice&) = delete;

  ~CompositeDevice();

  static zx_status_t Create(std::string_view name,
                            fuchsia_device_manager::wire::CompositeDeviceDescriptor comp_desc,
                            Coordinator& coordinator, std::unique_ptr<CompositeDevice>* out);

  static std::unique_ptr<CompositeDevice> CreateFromSpec(
      CompositeNodeSpecInfo driver_info, fbl::Array<std::unique_ptr<Metadata>> metadata,
      Coordinator& coordinator);

  // Attempt to match and bind any of the unbound fragments against |dev|.
  zx_status_t TryMatchBindFragments(const fbl::RefPtr<Device>& dev);

  // Bind the fragment with the given index to the specified device
  zx_status_t BindFragment(size_t index, const fbl::RefPtr<Device>& dev);

  // Should only be called if |driver_| is nullopt. Sets |driver_| and tries
  // to assemble the composite device.
  void SetMatchedDriver(MatchedDriverInfo driver);

  // Mark the given fragment as unbound.  Note that since we don't expose
  // this device's fragments in the API, this method can only be invoked by
  // CompositeDeviceFragment.
  void UnbindFragment(CompositeDeviceFragment* fragment);

  // Creates the actual device and orchestrates the creation of the composite
  // device in a driver_host.
  // Returns ZX_ERR_SHOULD_WAIT if some fragment is not fully ready (i.e. has
  // either not been matched or the fragment driver that bound to it has not
  // yet published its device).
  zx_status_t TryAssemble();

  // Forget about the composite device that was constructed.  If TryAssemble()
  // is invoked after this, it will reassemble the device.
  void Remove();

  fuchsia_driver_development::wire::CompositeInfo GetCompositeInfo(fidl::AnyArena& arena) const;

  CompositeDeviceFragment* GetPrimaryFragment();
  const CompositeDeviceFragment* GetPrimaryFragment() const;

  bool HasDriver() const { return driver_.has_value(); }

  const fbl::String& name() const { return name_; }
  const fbl::Array<const zx_device_prop_t>& properties() const { return properties_; }
  const fbl::Array<const StrProperty>& str_properties() const { return str_properties_; }
  uint32_t fragments_count() const { return fragments_count_; }

  // Returns a reference to the constructed composite device, if it exists.
  fbl::RefPtr<Device> device() const { return device_; }

  FragmentList& fragments() { return fragments_; }
  const FragmentList& fragments() const { return fragments_; }

 private:
  // Get the driver host that the composite device will live in.
  // If the composite device does not have a driver host yet, this function
  // will create a new one.
  zx::result<fbl::RefPtr<DriverHost>> GetDriverHost();

  fuchsia_driver_development::wire::LegacyCompositeNodeInfo GetLegacyCompositeInfo(
      fidl::AnyArena& arena) const;

  fidl::VectorView<fuchsia_driver_development::wire::CompositeParentNodeInfo> GetParentInfo(
      fidl::AnyArena& arena) const;

  // Returns true if a fragment matches |dev|. Sets |*index_out| will be set to the
  // matching fragment.
  bool IsFragmentMatch(const fbl::RefPtr<Device>& dev, size_t* index_out) const;

  // Finds a matching driver for the CompositeDevice. If it finds one, set |driver_|
  // to it. Otherwise, return ZX_ERR_NOT_FOUND. Should only be called if |driver_| is nullopt.
  zx::result<> MatchDriverToComposite();

  const fbl::String name_;
  const fbl::Array<const zx_device_prop_t> properties_;
  const fbl::Array<const StrProperty> str_properties_;

  const uint32_t fragments_count_;
  const uint32_t primary_fragment_index_;

  // The colocate flag set by device_add_composite(). std::nullopt if CompositeDevice
  // is created from the driver index.
  const std::optional<bool> legacy_colocate_flag_;

  const fbl::Array<std::unique_ptr<Metadata>> metadata_;

  // Set true if CompositeDevice was created from CreateFromSpec().
  const bool from_composite_node_spec_;

  // The driver that binds to actual device created by CompositeDevice. Only set by
  // CreateFromSpec(), SetDriver(), or MatchDriverToComposite().
  std::optional<const MatchedDriverInfo> driver_;

  FragmentList fragments_;

  // The driver host that the composite device will be placed into.
  // This will only be set once in GetDriverHost().
  fbl::RefPtr<DriverHost> driver_host_;

  // Once the composite has been assembled, this refers to the constructed
  // device.
  fbl::RefPtr<Device> device_;

  // Must outlive CompositeDevice.
  Coordinator& coordinator_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_COMPOSITE_DEVICE_H_
