// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_H_
#define SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_H_

#include <fidl/fuchsia.boot.metadata/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.usb.dci/cpp/fidl.h>
#include <fidl/fuchsia.hardware.usb.descriptor/cpp/wire.h>
#include <fidl/fuchsia.hardware.usb.endpoint/cpp/fidl.h>
#include <fidl/fuchsia.hardware.usb.phy/cpp/fidl.h>
#include <lib/async/cpp/irq.h>
#include <lib/dma-buffer/buffer.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/metadata/cpp/metadata_server.h>
#include <lib/driver/platform-device/cpp/pdev.h>
#include <lib/mmio/mmio.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/result.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/threads.h>

#include <cstdint>
#include <deque>
#include <memory>
#include <mutex>

#include <usb-endpoint/sdk/usb-endpoint-server.h>
#include <usb/descriptors.h>
#include <usb/sdk/request-fidl.h>

#include "src/devices/usb/drivers/dwc3/dwc3-event-fifo.h"
#include "src/devices/usb/drivers/dwc3/dwc3-trb-fifo.h"
#include "src/devices/usb/drivers/dwc3/dwc3-types.h"

namespace dwc3 {

// An extension class to extend the driver with SoC specific behavior for things such as power
// management.
class PlatformExtension {
 public:
  virtual ~PlatformExtension() = default;
  virtual zx::result<> Start() = 0;
  virtual zx::result<> Suspend() = 0;
  virtual zx::result<> Resume() = 0;
};

class Dwc3 : public fdf::DriverBase, public fidl::Server<fuchsia_hardware_usb_dci::UsbDci> {
 public:
  using fdf::DriverBase::incoming;

  Dwc3(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase{"dwc3", std::move(start_args), std::move(dispatcher)} {}
  ~Dwc3() { FDF_LOG(INFO, "~Dwc3()"); }

  zx::result<> Start() override;
  void Stop() override;

  // fuchsia_hardware_usb_dci::UsbDci protocol implementation.
  void ConnectToEndpoint(ConnectToEndpointRequest& request,
                         ConnectToEndpointCompleter::Sync& completer) override;

  void SetInterface(SetInterfaceRequest& request, SetInterfaceCompleter::Sync& completer) override;

  void StartController(StartControllerCompleter::Sync& completer) override;

  void StopController(StopControllerCompleter::Sync& completer) override;

  void ConfigureEndpoint(ConfigureEndpointRequest& request,
                         ConfigureEndpointCompleter::Sync& completer) override;

  void DisableEndpoint(DisableEndpointRequest& request,
                       DisableEndpointCompleter::Sync& completer) override;

  void EndpointSetStall(EndpointSetStallRequest& request,
                        EndpointSetStallCompleter::Sync& completer) override;

  void EndpointClearStall(EndpointClearStallRequest& request,
                          EndpointClearStallCompleter::Sync& completer) override;

  void CancelAll(CancelAllRequest& request, CancelAllCompleter::Sync& completer) override;

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_usb_dci::UsbDci> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override { /* no-op */ }

 private:
  const std::string_view kScheduleProfileRole = "fuchsia.devices.usb.drivers.dwc3.interrupt";
  static inline const uint32_t kEp0BufferSize = UINT16_MAX + 1;

  // physical endpoint numbers.  We use 0 and 1 for EP0, and let the device-mode
  // driver use the rest.
  static inline constexpr uint8_t kEp0Out = 0;
  static inline constexpr uint8_t kEp0In = 1;
  static inline constexpr uint8_t kUserEndpointStartNum = 2;
  static inline constexpr size_t kEp0MaxPacketSize = 512;

  static inline constexpr zx::duration kHwResetTimeout{zx::msec(50)};
  static inline constexpr zx::duration kEndpointDeadline{zx::sec(10)};

  struct UserEndpoint;
  class EpServer : public usb::EndpointServer {
   public:
    EpServer(const zx::bti& bti, Dwc3* dwc3, UserEndpoint* uep)
        : usb::EndpointServer{bti, uep->ep.ep_num}, dwc3_{dwc3}, uep_{uep} {}

    void CancelAll(zx_status_t reason);

    std::queue<usb::RequestVariant> queued_reqs;     // requests waiting to be processed
    std::optional<usb::RequestVariant> current_req;  // request currently being processed (if any)

   private:
    // EndpointServer overrides
    void OnUnbound(fidl::UnbindInfo info,
                   fidl::ServerEnd<fuchsia_hardware_usb_endpoint::Endpoint> server_end) override {
      CancelAll(ZX_ERR_IO_NOT_PRESENT);
      usb::EndpointServer::OnUnbound(info, std::move(server_end));
    }

    // fuchsia_hardware_usb_endpoint::Endpoint protocol implementation.
    void GetInfo(GetInfoCompleter::Sync& completer) override;
    void QueueRequests(QueueRequestsRequest& request,
                       QueueRequestsCompleter::Sync& completer) override;
    void CancelAll(CancelAllCompleter::Sync& completer) override;

    Dwc3* dwc3_{nullptr};         // Must outlive this instance.
    UserEndpoint* uep_{nullptr};  // Must outlive this instance.
  };

  struct Endpoint {
    Endpoint() = default;
    explicit Endpoint(uint8_t ep_num) : ep_num(ep_num) {}

    Endpoint(const Endpoint&) = delete;
    Endpoint& operator=(const Endpoint&) = delete;
    Endpoint(Endpoint&&) = delete;
    Endpoint& operator=(Endpoint&&) = delete;

    static inline constexpr bool IsOutput(uint8_t ep_num) { return (ep_num & 0x1) == 0; }
    static inline constexpr bool IsInput(uint8_t ep_num) { return (ep_num & 0x1) == 1; }

    bool IsOutput() const { return IsOutput(ep_num); }
    bool IsInput() const { return IsInput(ep_num); }

    uint32_t rsrc_id{0};  // resource ID for current_req

    const uint8_t ep_num{0};
    uint8_t type{0};  // control, bulk, interrupt or isochronous
    uint8_t interval{0};
    uint16_t max_packet_size{0};
    bool enabled{false};
    // TODO(voydanoff) USB 3 specific stuff here

    bool got_not_ready{false};
    bool stalled{false};
  };

  struct UserEndpoint {
    UserEndpoint() = default;
    UserEndpoint(const UserEndpoint&) = delete;
    UserEndpoint& operator=(const UserEndpoint&) = delete;
    UserEndpoint(UserEndpoint&&) = delete;
    UserEndpoint& operator=(UserEndpoint&&) = delete;

    TrbFifo fifo;
    Endpoint ep;
    std::optional<EpServer> server;
  };

  // A small helper class which basically allows us to have a collection of user
  // endpoints which is dynamically allocated at startup, but which will never
  // change in size.  std::array is not an option here, as it is sized at
  // compile time, while std::vector would force us to make user endpoints
  // movable objects (which we really don't want to do).  Basically, this is a
  // lot of typing to get a C-style array which knows its size and supports
  // range based iteration.
  class UserEndpointCollection {
   public:
    void Init(size_t count, const zx::bti& bti, Dwc3* dwc3) {
      ZX_DEBUG_ASSERT(count <= (std::numeric_limits<uint8_t>::max() - kUserEndpointStartNum));
      ZX_DEBUG_ASSERT(count_ == 0);
      ZX_DEBUG_ASSERT(endpoints_.get() == nullptr);

      count_ = count;
      endpoints_ = std::make_unique<UserEndpoint[]>(count_);
      for (size_t i = 0; i < count_; ++i) {
        UserEndpoint& uep = endpoints_[i];
        const_cast<uint8_t&>(uep.ep.ep_num) = static_cast<uint8_t>(i) + kUserEndpointStartNum;
        uep.server.emplace(bti, dwc3, &uep);
      }
    }

    // Standard size and index-operator
    size_t size() const { return count_; }
    UserEndpoint& operator[](size_t ndx) {
      ZX_DEBUG_ASSERT(ndx < count_);
      return endpoints_[ndx];
    }

    // Support for range-based for loops.
    UserEndpoint* begin() { return endpoints_.get() + 0; }
    UserEndpoint* end() { return endpoints_.get() + count_; }
    const UserEndpoint* begin() const { return endpoints_.get() + 0; }
    const UserEndpoint* end() const { return endpoints_.get() + count_; }

   private:
    size_t count_{0};
    std::unique_ptr<UserEndpoint[]> endpoints_;
  };

  struct Ep0 {
    Ep0() : out(kEp0Out), in(kEp0In) {}

    Ep0(const Ep0&) = delete;
    Ep0& operator=(const Ep0&) = delete;
    Ep0(Ep0&&) = delete;
    Ep0& operator=(Ep0&&) = delete;

    enum class State {
      None,
      Setup,        // Queued setup phase
      DataOut,      // Queued data on EP0_OUT
      DataIn,       // Queued data on EP0_IN
      WaitNrdyOut,  // Waiting for not-ready on EP0_OUT
      WaitNrdyIn,   // Waiting for not-ready on EP0_IN
      Status,       // Waiting for status to complete
    };

    TrbFifo shared_fifo;
    std::unique_ptr<dma_buffer::ContiguousBuffer> buffer;
    State state = Ep0::State::None;
    Endpoint out;
    Endpoint in;
    fuchsia_hardware_usb_descriptor::wire::UsbSetup cur_setup;
    fuchsia_hardware_usb_descriptor::wire::UsbSpeed cur_speed{
        fuchsia_hardware_usb_descriptor::wire::UsbSpeed::kUndefined};
    bool transfer_in_progress_ = false;
  };

  constexpr bool is_ep0_num(uint8_t ep_num) { return ((ep_num == kEp0Out) || (ep_num == kEp0In)); }

  UserEndpoint* get_user_endpoint(uint8_t ep_num) {
    if (ep_num >= kUserEndpointStartNum) {
      const uint8_t ndx = ep_num - kUserEndpointStartNum;
      return (ndx < user_endpoints_.size()) ? &user_endpoints_[ndx] : nullptr;
    }
    return nullptr;
  }

  fdf::MmioBuffer* get_mmio() { return &*mmio_; }
  static uint8_t UsbAddressToEpNum(uint8_t addr) {
    return static_cast<uint8_t>(((addr & 0xF) << 1) | !!(addr & USB_DIR_IN));
  }

  zx_status_t AcquirePDevResources();
  zx_status_t Init();
  void ReleaseResources();

  // IRQ thread's two top level event decoders.
  void HandleEvent(uint32_t event);
  void HandleEpEvent(uint32_t event);

  // Handlers for global events posted to the event buffer by the controller HW.
  void HandleResetEvent();
  void HandleConnectionDoneEvent();
  void HandleDisconnectedEvent();

  // Handlers for end-point specific events posted to the event buffer by the controller HW.
  void HandleEpTransferCompleteEvent(uint8_t ep_num);
  void HandleEpTransferNotReadyEvent(uint8_t ep_num, uint32_t stage);
  void HandleEpTransferStartedEvent(uint8_t ep_num, uint32_t rsrc_id);

  [[nodiscard]] zx_status_t CheckHwVersion();
  [[nodiscard]] zx_status_t ResetHw();
  void StartEvents();
  void SetDeviceAddress(uint32_t address);

  // EP0 stuff
  zx_status_t Ep0Init();
  void Ep0Reset();
  void Ep0Start();
  void Ep0QueueSetup();
  void Ep0StartEndpoints();
  void HandleEp0Setup(size_t length);
  void HandleEp0TransferCompleteEvent(uint8_t ep_num);
  void HandleEp0TransferNotReadyEvent(uint8_t ep_num, uint32_t stage);

  // General EP stuff
  void EpEnable(Endpoint& ep, bool enable);
  void EpSetConfig(Endpoint& ep, bool enable);
  zx_status_t EpSetStall(Endpoint& ep, bool stall);
  void EpStartTransfer(Endpoint& ep, TrbFifo& fifo, uint32_t type, zx_paddr_t buffer,
                       size_t length);
  void EpReset(Endpoint& ep);

  // Methods specific to user endpoints
  void UserEpReset(UserEndpoint& uep);
  void UserEpQueueNext(UserEndpoint& uep);

  void ResetEndpoints();

  // Commands
  void CmdStartNewConfig(const Endpoint& ep, uint32_t rsrc_id);
  void CmdEpSetConfig(const Endpoint& ep, bool modify);
  void CmdEpTransferConfig(const Endpoint& ep);
  void CmdEpStartTransfer(const Endpoint& ep, zx_paddr_t trb_phys);
  void CmdEpEndTransfer(const Endpoint& ep);
  void CmdEpSetStall(const Endpoint& ep);
  void CmdEpClearStall(const Endpoint& ep);

  // Start to operate in peripheral mode.
  void StartPeripheralMode();
  void ResetConfiguration();

  void HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                 const zx_packet_interrupt_t* interrupt);

  fdf::PDev pdev_;

  fidl::WireClient<fuchsia_hardware_usb_dci::UsbDciInterface> dci_intf_;

  std::optional<ddk::MmioBuffer> mmio_;

  zx::bti bti_;
  bool has_pinned_memory_{false};

  zx::interrupt irq_;

  EventFifo event_fifo_;
  async::IrqMethod<Dwc3, &Dwc3::HandleIrq> irq_handler_{this};

  Ep0 ep0_;
  UserEndpointCollection user_endpoints_;

  std::unique_ptr<PlatformExtension> platform_extension_;

  fidl::ServerBindingGroup<fuchsia_hardware_usb_dci::UsbDci> bindings_;
  fidl::SyncClient<fuchsia_driver_framework::NodeController> child_;

  fdf_metadata::MetadataServer<fuchsia_boot_metadata::MacAddressMetadata>
      mac_address_metadata_server_;
  fdf_metadata::MetadataServer<fuchsia_boot_metadata::SerialNumberMetadata>
      serial_number_metadata_server_;
  fdf_metadata::MetadataServer<fuchsia_hardware_usb_phy::Metadata> usb_phy_metadata_server_;
};

}  // namespace dwc3

#endif  // SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_H_
