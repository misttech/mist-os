// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_H_
#define SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.usb.dci/cpp/fidl.h>
#include <fidl/fuchsia.hardware.usb.descriptor/cpp/wire.h>
#include <fidl/fuchsia.hardware.usb.endpoint/cpp/fidl.h>
#include <lib/dma-buffer/buffer.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/platform-device/cpp/pdev.h>
#include <lib/mmio/mmio.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/result.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <cstdint>
#include <deque>
#include <memory>
#include <mutex>

#include <usb-endpoint/usb-endpoint-server.h>
#include <usb/descriptors.h>
#include <usb/request-fidl.h>

#include "src/devices/usb/drivers/dwc3/dwc3-types.h"

namespace dwc3 {

// A dma_buffer::ContiguousBuffer is cached, but leaves cache management to the user. These methods
// wrap zx_cache_flush with sensible boundary checking and validation.
zx_status_t CacheFlush(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset, size_t length);
zx_status_t CacheFlushInvalidate(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset,
                                 size_t length);

class Dwc3 : public fdf::DriverBase, public fidl::Server<fuchsia_hardware_usb_dci::UsbDci> {
 public:
  Dwc3(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase{"dwc3", std::move(start_args), std::move(dispatcher)} {}

  zx::result<> Start() override;
  void Stop() override;

  // fuchsia_hardware_usb_dci::UsbDci protocol implementation.
  void ConnectToEndpoint(ConnectToEndpointRequest& request,
                         ConnectToEndpointCompleter::Sync& completer) override;

  void SetInterface(SetInterfaceRequest& request, SetInterfaceCompleter::Sync& completer) override;

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
  zx::result<> CommonCancelAll(uint8_t ep_addr);
  void DciIntfSetSpeed(fuchsia_hardware_usb_descriptor::wire::UsbSpeed speed)
      __TA_REQUIRES(dci_lock_);
  void DciIntfSetConnected(bool connected) __TA_REQUIRES(dci_lock_);
  zx::result<size_t> DciIntfControl(const fuchsia_hardware_usb_descriptor::wire::UsbSetup* setup,
                                    const uint8_t* write_buffer, size_t write_size,
                                    uint8_t* read_buffer, size_t read_size)
      __TA_REQUIRES(dci_lock_);

  static inline const uint32_t kEventBufferSize = zx_system_get_page_size();
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
  struct RequestInfo {
    size_t actual;
    zx_status_t status;
    UserEndpoint* uep;
    usb_endpoint::RequestVariant req;
  };

  // Like a usb::RequestQueue for usb::FidlRequests.
  //
  // TODO(hansens) We should probably implement something like this in usb/request-fidl.h
  class FidlRequestQueue {
   public:
    FidlRequestQueue() = default;

    // Movable, not copyable.
    FidlRequestQueue(FidlRequestQueue&& other) {
      std::lock_guard<std::mutex> lock(lock_);
      q_ = std::move(other.q_);
      other.q_.clear();
    }

    FidlRequestQueue& operator=(FidlRequestQueue&& other) {
      std::lock_guard<std::mutex> other_lock(other.lock_);
      std::lock_guard<std::mutex> my_lock(lock_);
      q_ = std::move(other.q_);
      other.q_.clear();
      return *this;
    }

    FidlRequestQueue(const FidlRequestQueue&) = delete;
    FidlRequestQueue& operator=(const FidlRequestQueue&) = delete;

    void push(RequestInfo&& info) {
      std::lock_guard<std::mutex> lock(lock_);

      if (info.uep == nullptr) {
        FDF_LOG(ERROR, "[BUG] Enqueuing usb::FidlRequest with no corresponding uep");
      }
      ZX_ASSERT(info.uep != nullptr);  // Halt and catch fire.

      q_.push_front(std::move(info));
    }

    // Pushes to the (semantic) front of the queue, cutting the line.
    void push_next(RequestInfo&& info) {
      std::lock_guard<std::mutex> lock(lock_);
      q_.push_back(std::move(info));
    }

    // Pop the next value from the queue, if any.
    std::optional<RequestInfo> pop() {
      std::lock_guard<std::mutex> lock(lock_);
      std::optional<RequestInfo> opt{std::nullopt};

      if (!q_.empty()) {
        opt.emplace(std::move(q_.back()));
        q_.pop_back();
      }

      return opt;
    }

    bool empty() const {
      std::lock_guard<std::mutex> lock(lock_);
      return q_.empty();
    }

    void CompleteAll(zx_status_t status, size_t size) {
      std::lock_guard<std::mutex> _(lock_);

      while (!q_.empty()) {
        RequestInfo info{std::move(q_.back())};
        q_.pop_back();
        info.uep->server->RequestComplete(status, size, std::move(info.req));
      }
    }

   private:
    mutable std::mutex lock_;
    std::deque<RequestInfo> q_ __TA_GUARDED(lock_);  // Queued front-to-back.
  };

  enum class IrqSignal : uint32_t {
    Invalid = 0,
    Exit = 1,
    Wakeup = 2,
  };

  struct Fifo {
    static inline const uint32_t kFifoSize = zx_system_get_page_size();

    zx_status_t Init(zx::bti& bti);
    void Release();

    zx_paddr_t GetTrbPhys(dwc3_trb_t* trb) const {
      ZX_DEBUG_ASSERT((trb >= first) && (trb <= last));
      return buffer->phys() + ((trb - first) * sizeof(*trb));
    }

    std::unique_ptr<dma_buffer::ContiguousBuffer> buffer;
    dwc3_trb_t* first{nullptr};    // first TRB in the fifo
    dwc3_trb_t* next{nullptr};     // next free TRB in the fifo
    dwc3_trb_t* current{nullptr};  // TRB for currently pending transaction
    dwc3_trb_t* last{nullptr};     // last TRB in the fifo (link TRB)
  };

  class EpServer : public usb_endpoint::UsbEndpoint {
   public:
    EpServer(const zx::bti& bti, Dwc3* dwc3, UserEndpoint* uep)
        : usb_endpoint::UsbEndpoint{bti, uep->ep.ep_num}, dwc3_{dwc3}, uep_{uep} {
      auto result =
          fdf::SynchronizedDispatcher::Create({}, "ep-dispatcher", [&](fdf_dispatcher_t*) {});
      if (result.is_error()) {
        FDF_LOG(ERROR, "Could not initialize dispatcher: %s", result.status_string());
        return;
      }
      dispatcher_ = std::move(result.value());
    }

    ~EpServer() override {
      dispatcher_.ShutdownAsync();
      dispatcher_.release();
    }

    // fuchsia_hardware_usb_endpoint::Endpoint protocol implementation.
    void GetInfo(GetInfoCompleter::Sync& completer) override;
    void QueueRequests(QueueRequestsRequest& request,
                       QueueRequestsCompleter::Sync& completer) override;
    void CancelAll(CancelAllCompleter::Sync& completer) override;

    async_dispatcher_t* dispatcher() { return dispatcher_.async_dispatcher(); }

   private:
    Dwc3* dwc3_{nullptr};         // Must outlive this instance.
    UserEndpoint* uep_{nullptr};  // Must outlive this instance.
    fdf::SynchronizedDispatcher dispatcher_;
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

    FidlRequestQueue queued_reqs;            // requests waiting to be processed
    std::optional<RequestInfo> current_req;  // request currently being processed (if any)
    uint32_t rsrc_id{0};                     // resource ID for current_req

    const uint8_t ep_num{0};
    uint8_t type{0};  // control, bulk, interrupt or isochronous
    uint8_t interval{0};
    uint16_t max_packet_size{0};
    bool enabled{false};
    // TODO(voydanoff) USB 3 specific stuff here

    bool got_not_ready{false};
    bool stalled{false};

    // Used for synchronizing endpoint state and ep specific hardware registers
    // This should be acquired before Dwc3::lock_ if acquiring both locks.
    std::mutex lock;
  };

  struct UserEndpoint {
    UserEndpoint() = default;
    UserEndpoint(const UserEndpoint&) = delete;
    UserEndpoint& operator=(const UserEndpoint&) = delete;
    UserEndpoint(UserEndpoint&&) = delete;
    UserEndpoint& operator=(UserEndpoint&&) = delete;

    __TA_GUARDED(ep.lock) Fifo fifo;
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
        std::lock_guard<std::mutex> lock(uep.ep.lock);
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

    std::mutex lock;

    __TA_GUARDED(lock) Fifo shared_fifo;
    __TA_GUARDED(lock) std::unique_ptr<dma_buffer::ContiguousBuffer> buffer;
    __TA_GUARDED(lock) State state { Ep0::State::None };
    __TA_GUARDED(lock) Endpoint out;
    __TA_GUARDED(lock) Endpoint in;
    __TA_GUARDED(lock) fuchsia_hardware_usb_descriptor::wire::UsbSetup cur_setup;

    __TA_GUARDED(lock)
    fuchsia_hardware_usb_descriptor::wire::UsbSpeed cur_speed{
        fuchsia_hardware_usb_descriptor::wire::UsbSpeed::kUndefined};
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
  libsync::Completion init_done_;  // Signaled at the end of Init().
  void ReleaseResources();

  // The IRQ thread and its two top level event decoders.
  int IrqThread();
  void HandleEvent(uint32_t event);
  void HandleEpEvent(uint32_t event);

  [[nodiscard]] zx_status_t SignalIrqThread(IrqSignal signal) {
    if (!irq_bound_to_port_) {
      return ZX_ERR_BAD_STATE;
    }

    zx_port_packet_t pkt{
        .key = 0,
        .type = ZX_PKT_TYPE_USER,
        .status = ZX_OK,
    };
    pkt.user.u32[0] = static_cast<std::underlying_type_t<decltype(signal)>>(signal);

    return irq_port_.queue(&pkt);
  }

  IrqSignal GetIrqSignal(const zx_port_packet_t& pkt) {
    if (pkt.type != ZX_PKT_TYPE_USER) {
      return IrqSignal::Invalid;
    }
    return static_cast<IrqSignal>(pkt.user.u32[0]);
  }

  // Handlers for global events posted to the event buffer by the controller HW.
  void HandleResetEvent() __TA_EXCLUDES(lock_);
  void HandleConnectionDoneEvent() __TA_EXCLUDES(lock_);
  void HandleDisconnectedEvent() __TA_EXCLUDES(lock_);

  // Handlers for end-point specific events posted to the event buffer by the controller HW.
  void HandleEpTransferCompleteEvent(uint8_t ep_num) __TA_EXCLUDES(lock_);
  void HandleEpTransferNotReadyEvent(uint8_t ep_num, uint32_t stage) __TA_EXCLUDES(lock_);
  void HandleEpTransferStartedEvent(uint8_t ep_num, uint32_t rsrc_id) __TA_EXCLUDES(lock_);

  [[nodiscard]] zx_status_t CheckHwVersion() __TA_REQUIRES(lock_);
  [[nodiscard]] zx_status_t ResetHw() __TA_REQUIRES(lock_);
  void StartEvents() __TA_REQUIRES(lock_);
  void SetDeviceAddress(uint32_t address) __TA_REQUIRES(lock_);

  // EP0 stuff
  zx_status_t Ep0Init() __TA_EXCLUDES(lock_);
  void Ep0Reset() __TA_EXCLUDES(lock_);
  void Ep0Start() __TA_EXCLUDES(lock_);
  void Ep0QueueSetupLocked() __TA_REQUIRES(ep0_.lock) __TA_EXCLUDES(lock_);
  void Ep0StartEndpoints() __TA_REQUIRES(ep0_.lock) __TA_EXCLUDES(lock_);
  zx::result<size_t> HandleEp0Setup(const fuchsia_hardware_usb_descriptor::wire::UsbSetup& setup,
                                    void* buffer, size_t length) __TA_REQUIRES(ep0_.lock)
      __TA_EXCLUDES(lock_);
  void HandleEp0TransferCompleteEvent(uint8_t ep_num) __TA_EXCLUDES(lock_, ep0_.lock);
  void HandleEp0TransferNotReadyEvent(uint8_t ep_num, uint32_t stage)
      __TA_EXCLUDES(lock_, ep0_.lock);

  // General EP stuff
  void EpEnable(const Endpoint& ep, bool enable) __TA_EXCLUDES(lock_);
  void EpSetConfig(Endpoint& ep, bool enable) __TA_EXCLUDES(lock_);
  zx_status_t EpSetStall(Endpoint& ep, bool stall) __TA_EXCLUDES(lock_);
  void EpStartTransfer(Endpoint& ep, Fifo& fifo, uint32_t type, zx_paddr_t buffer, size_t length,
                       bool send_zlp) __TA_EXCLUDES(lock_);
  void EpEndTransfers(Endpoint& ep, zx_status_t reason) __TA_EXCLUDES(lock_);
  void EpReadTrb(Endpoint& ep, Fifo& fifo, const dwc3_trb_t* src, dwc3_trb_t* dst)
      __TA_EXCLUDES(lock_);

  // Methods specific to user endpoints
  void UserEpQueueNext(UserEndpoint& uep) __TA_REQUIRES(uep.ep.lock) __TA_EXCLUDES(lock_);
  zx_status_t UserEpCancelAll(UserEndpoint& uep) __TA_EXCLUDES(lock_, uep.ep.lock);

  // Cancel all currently in flight requests, and return a list of requests
  // which were in-flight.  Note that these requests have not been completed
  // yet.  It is the responsibility of the caller to (eventually) take care of
  // this once the lock has been dropped.
  [[nodiscard]] FidlRequestQueue UserEpCancelAllLocked(UserEndpoint& uep) __TA_REQUIRES(uep.ep.lock)
      __TA_EXCLUDES(lock_);

  // Commands
  void CmdStartNewConfig(const Endpoint& ep, uint32_t rsrc_id) __TA_EXCLUDES(lock_);
  void CmdEpSetConfig(const Endpoint& ep, bool modify) __TA_EXCLUDES(lock_);
  void CmdEpTransferConfig(const Endpoint& ep) __TA_EXCLUDES(lock_);
  void CmdEpStartTransfer(const Endpoint& ep, zx_paddr_t trb_phys) __TA_EXCLUDES(lock_);
  void CmdEpEndTransfer(const Endpoint& ep) __TA_EXCLUDES(lock_);
  void CmdEpSetStall(const Endpoint& ep) __TA_EXCLUDES(lock_);
  void CmdEpClearStall(const Endpoint& ep) __TA_EXCLUDES(lock_);

  // Start to operate in peripheral mode.
  void StartPeripheralMode() __TA_EXCLUDES(lock_);
  void ResetConfiguration() __TA_EXCLUDES(lock_);

  std::mutex lock_;
  std::mutex dci_lock_;

  fdf::PDev pdev_;

  fidl::WireSyncClient<fuchsia_hardware_usb_dci::UsbDciInterface> dci_intf_ __TA_GUARDED(dci_lock_);

  std::optional<ddk::MmioBuffer> mmio_;

  zx::bti bti_;
  bool has_pinned_memory_{false};

  zx::interrupt irq_;
  zx::port irq_port_;
  bool irq_bound_to_port_{false};

  thrd_t irq_thread_;
  std::atomic<bool> irq_thread_started_{false};

  std::unique_ptr<dma_buffer::ContiguousBuffer> event_buffer_;

  Ep0 ep0_;
  UserEndpointCollection user_endpoints_;

  FidlRequestQueue pending_completions_;

  // TODO(johngro): What lock protects this?  Right now, it is effectively
  // endpoints_[0].lock(), but how do we express this?
  bool configured_ = false;

  fidl::ServerBindingGroup<fuchsia_hardware_usb_dci::UsbDci> bindings_;
  fidl::SyncClient<fuchsia_driver_framework::NodeController> child_;

  compat::SyncInitializedDeviceServer compat_;
};

}  // namespace dwc3

#endif  // SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_H_
