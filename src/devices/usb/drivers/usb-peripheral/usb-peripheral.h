// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_USB_PERIPHERAL_USB_PERIPHERAL_H_
#define SRC_DEVICES_USB_DRIVERS_USB_PERIPHERAL_USB_PERIPHERAL_H_

#include <fidl/fuchsia.hardware.usb.dci/cpp/wire.h>
#include <fidl/fuchsia.hardware.usb.peripheral/cpp/wire.h>
#include <fuchsia/hardware/usb/dci/cpp/banjo.h>
#include <fuchsia/hardware/usb/function/cpp/banjo.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/zx/channel.h>
#include <zircon/errors.h>

#include <utility>

#include <fbl/mutex.h>
#include <usb-monitor-util/usb-monitor-util.h>
#include <usb/request-cpp.h>

#include "src/devices/usb/drivers/usb-peripheral/usb-dci-interface-server.h"
#include "src/devices/usb/drivers/usb-peripheral/usb_peripheral_config.h"

/*
    THEORY OF OPERATION

    This driver is responsible for USB in the peripheral role, that is,
    acting as a USB device to a USB host.
    It serves as the central point of coordination for the peripheral role.
    It is configured via ioctls in the fuchsia.hardware.usb.peripheral FIDL interface
    (which is used by the usbctl command line program).
    Based on this configuration, it creates one or more devmgr devices with protocol
    ZX_PROTOCOL_USB_FUNCTION. These devices are bind points for USB function drivers,
    which implement USB interfaces for particular functions (like USB ethernet or mass storage).
    This driver also binds to a device with protocol ZX_PROTOCOL_USB_DCI
    (Device Controller Interface) which is implemented by a driver for the actual
    USB controller hardware for the peripheral role.

    The FIDL interface SetConfiguration() is used to initialize and start USB in the
    peripheral role. Internally this consists of several steps.
    The first step is setting up the USB device descriptor to be presented to the host
    during enumeration.
    Next, the descriptors for the USB functions are added to the configuration.
    Finally after all the functions have been added, the configuration is complete and
    it is now possible to build the configuration descriptor.
    Once we get to this point, UsbPeripheral.functions_bound_ is set to true.

    If the role is set to USB_MODE_PERIPHERAL and functions_bound_ is true,
    then we are ready to start USB in peripheral role.
    At this point, we create DDK devices for our list of functions.
    When the function drivers bind to these functions, they register an interface of type
    usb_function_interface_protocol_t with this driver via the usb_function_register() API.
    Once all of the function drivers have registered themselves this way,
    UsbPeripheral.functions_registered_ is set to true.

    if the usb mode is set to USB_MODE_PERIPHERAL and functions_registered_ is true,
    we are now finally ready to operate in the peripheral role.

    Teardown of the peripheral role:
    The FIDL ClearFunctions() message will reset this device's list of USB functions.
*/

namespace usb_peripheral {

class UsbFunction;

using ConfigurationDescriptor =
    ::fidl::VectorView<fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor>;
using fuchsia_hardware_usb_peripheral::wire::DeviceDescriptor;
using fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor;

struct UsbConfiguration {
  explicit UsbConfiguration(uint8_t index) : index(index) {}

  static constexpr uint8_t MAX_INTERFACES = 32;
  // Indices of the functions associated with this configuration
  std::vector<size_t> functions;
  // USB configuration descriptor, synthesized from our functions' descriptors.
  std::vector<uint8_t> config_desc;

  // Map from interface number to function index.
  std::optional<size_t> interface_map[MAX_INTERFACES];
  const uint8_t index;
};

// This is the main class for the USB peripheral role driver.
// It binds against the USB DCI driver device and manages a list of UsbFunction devices,
// one for each USB function in the peripheral role configuration.
class UsbPeripheral : public fdf::DriverBase,
                      public fidl::WireServer<fuchsia_hardware_usb_peripheral::Device>,
                      public ddk::UsbDciInterfaceProtocol<UsbPeripheral> {
 public:
  static constexpr std::string_view kDriverName = "usb_device";
  static constexpr std::string_view kChildNodeName = "usb-peripheral";

  UsbPeripheral(fdf::DriverStartArgs start_args,
                fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  // fdf::DriverBase implementation.
  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;
  void Stop() override;

  // UsbDciInterface implementation.
  zx_status_t UsbDciInterfaceControl(const usb_setup_t* setup, const uint8_t* write_buffer,
                                     size_t write_size, uint8_t* out_read_buffer, size_t read_size,
                                     size_t* out_read_actual);
  void UsbDciInterfaceSetConnected(bool connected);
  void UsbDciInterfaceSetSpeed(usb_speed_t speed);
  zx_status_t UsbDciCancelAll(uint8_t ep_address);

  // fuchsia_hardware_usb_peripheral::Device protocol implementation.
  void SetConfiguration(SetConfigurationRequestView request,
                        SetConfigurationCompleter::Sync& completer) override;
  void ClearFunctions(ClearFunctionsCompleter::Sync& completer) override;
  void SetStateChangeListener(SetStateChangeListenerRequestView request,
                              SetStateChangeListenerCompleter::Sync& completer) override;

  zx_status_t SetDeviceDescriptor(DeviceDescriptor desc);
  zx_status_t AllocInterface(size_t function_index, uint8_t* out_intf_num);
  zx_status_t AllocEndpoint(size_t function_index, uint8_t direction, uint8_t* out_address);
  zx_status_t AllocStringDesc(std::string desc, uint8_t* out_index);
  zx_status_t ValidateFunction(size_t function_index, void* descriptors, size_t length,
                               uint8_t* out_num_interfaces);
  zx_status_t FunctionRegistered();
  void FunctionCleared();

  inline const ddk::UsbDciProtocolClient& dci() const { return dci_; }
  inline const fidl::WireSyncClient<fuchsia_hardware_usb_dci::UsbDci>& dci_new() const {
    return dci_new_;
  }
  inline bool dci_new_valid() const { return dci_new_valid_; }

  inline size_t ParentRequestSize() const { return parent_request_size_; }
  void UsbPeripheralRequestQueue(usb_request_t* usb_request,
                                 const usb_request_complete_callback_t* complete_cb);

  zx_status_t ConnectToEndpoint(uint8_t ep_address,
                                fidl::ServerEnd<fuchsia_hardware_usb_endpoint::Endpoint> ep) {
    auto result = dci_new_->ConnectToEndpoint(ep_address, std::move(ep));
    if (!result.ok()) {
      return ZX_ERR_INTERNAL;  // framework error.
    } else if (result->is_error()) {
      return result->error_value();
    }
    return ZX_OK;
  }

  const usb_device_descriptor_t& device_desc() { return device_desc_; }
  zx_status_t DeviceStateChanged();

 private:
  // Considered part of the private impl.
  friend class UsbDciInterfaceServer;

  // For the purposes of banjo->FIDL migration. Once banjo is ripped out of the driver, the logic
  // here can be folded into the FIDL endpoint implementation and calling code.
  zx_status_t CommonControl(const usb_setup_t* setup, const uint8_t* write_buffer,
                            size_t write_size, uint8_t* read_buffer, size_t read_size,
                            size_t* out_read_actual);
  void CommonSetConnected(bool connected);
  // SetSpeed() is trivial and warrants no common impl.

  zx_status_t StartController();
  zx_status_t StopController();

  DISALLOW_COPY_ASSIGN_AND_MOVE(UsbPeripheral);

  static constexpr uint8_t MAX_STRINGS = 255;
  static constexpr uint8_t MAX_STRING_LENGTH = 126;

  // OUT endpoints are in range 1 - 15, IN endpoints are in range 17 - 31.
  static constexpr uint8_t OUT_EP_START = 1;
  static constexpr uint8_t OUT_EP_END = 15;
  static constexpr uint8_t IN_EP_START = 17;
  static constexpr uint8_t IN_EP_END = 31;

  // For mapping b_endpoint_address value to/from index in range 0 - 31.
  static inline uint8_t EpAddressToIndex(uint8_t addr) {
    return static_cast<uint8_t>(((addr) & 0xF) | (((addr) & 0x80) >> 3));
  }
  static inline uint8_t EpIndexToAddress(uint8_t index) {
    return static_cast<uint8_t>(((index) & 0xF) | (((index) & 0x10) << 3));
  }

  // Returns the index of the function that was added.
  zx::result<size_t> AddFunction(UsbConfiguration& config, FunctionDescriptor desc);
  // Begins the process of clearing the functions.
  void ClearFunctions();
  zx::result<std::string> GetSerialNumber();
  zx_status_t DeviceStateChangedLocked() __TA_REQUIRES(lock_);
  zx_status_t AddFunctionDevices() __TA_REQUIRES(lock_);
  zx_status_t GetDescriptor(uint8_t request_type, uint16_t value, uint16_t index, void* buffer,
                            size_t length, size_t* out_actual);
  zx_status_t SetConfiguration(uint8_t configuration);
  zx_status_t SetInterface(uint8_t interface, uint8_t alt_setting);
  zx_status_t SetDefaultConfig(std::vector<FunctionDescriptor>& functions);
  int ListenerCleanupThread();
  void RequestComplete(usb_request_t* req);

  bool AllFunctionsRegistered() const;
  UsbFunction& GetFunction(size_t index);
  const UsbFunction& GetFunction(size_t index) const;

  void Connect(fidl::ServerEnd<fuchsia_hardware_usb_peripheral::Device> request) {
    bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
  }

  // `UsbFunction` wrapped in `unique_ptr` because `UsbFunction` instance may be bound as a FIDL
  // server which requires that the instance always be at the same memory address.
  std::vector<std::unique_ptr<UsbFunction>> functions_;

  // Our parent's DCI protocol.
  ddk::UsbDciProtocolClient dci_;
  bool dci_new_valid_ = false;
  fidl::WireSyncClient<fuchsia_hardware_usb_dci::UsbDci> dci_new_;
  // USB device descriptor set via ioctl_usb_peripheral_set_device_desc()
  usb_device_descriptor_t device_desc_ = {};
  // Map from endpoint index to function index.
  std::optional<size_t> endpoint_map_[USB_MAX_EPS];
  // Strings for USB string descriptors.
  std::vector<std::string> strings_ __TA_GUARDED(lock_);
  // List of usb_function_t.
  std::vector<UsbConfiguration> configurations_;
  // mutex for protecting our state
  fbl::Mutex lock_;
  // Current USB mode set via ioctl_usb_peripheral_set_mode()
  usb_mode_t cur_usb_mode_ __TA_GUARDED(lock_) = USB_MODE_NONE;
  // Our parent's USB mode. Should not change after being set.
  usb_mode_t parent_usb_mode_ __TA_GUARDED(lock_) = USB_MODE_NONE;
  // |lock_functions_|: true if all functions have been added to configurations_ and should not be
  // changed any more.
  bool lock_functions_ __TA_GUARDED(lock_) = false;
  // True if we have added child devices for our functions.
  bool function_devs_added_ __TA_GUARDED(lock_) = false;
  // True if fuchsia_hardware_usb_dci::SetInterface performed in Init().
  bool set_interface_in_init_ __TA_GUARDED(lock_) = false;
  // Number of functions left to clear.
  size_t num_functions_to_clear_ __TA_GUARDED(lock_) = 0;
  // True if we are connected to a host,
  bool connected_ __TA_GUARDED(lock_) = false;
  // True if we are shutting down/clearing functions
  bool shutting_down_ = false;
  // Current configuration number selected via USB_REQ_SET_CONFIGURATION
  // (will be 0 or 1 since we currently do not support multiple configurations).
  // 0 indicates that the device is unconfigured and should not accept USB requests
  // other than USB_REQ_SET_CONFIGURATION or requests targetting descriptors
  uint8_t configuration_ = 0;
  // USB connection speed.
  usb_speed_t speed_ = 0;
  // Size of our parent's usb_request_t, only relevant to the banjo interface.
  size_t parent_request_size_ = 0;
  // Registered listener
  fidl::ClientEnd<fuchsia_hardware_usb_peripheral::Events> listener_;

  thrd_t thread_ = 0;

  bool cache_enabled_ = true;
  bool cache_report_enabled_ = true;

  UsbMonitor usb_monitor_;

  fbl::Mutex pending_requests_lock_;
  usb::BorrowedRequestList<void> pending_requests_ __TA_GUARDED(pending_requests_lock_);

  UsbDciInterfaceServer intf_srv_{this};

  fidl::ServerBindingGroup<fuchsia_hardware_usb_peripheral::Device> bindings_;
  fdf::OwnedChildNode child_;
  driver_devfs::Connector<fuchsia_hardware_usb_peripheral::Device> devfs_connector_{
      fit::bind_member<&UsbPeripheral::Connect>(this)};
  std::optional<fdf::PrepareStopCompleter> prepare_stop_completer_;
};

}  // namespace usb_peripheral

#endif  // SRC_DEVICES_USB_DRIVERS_USB_PERIPHERAL_USB_PERIPHERAL_H_
