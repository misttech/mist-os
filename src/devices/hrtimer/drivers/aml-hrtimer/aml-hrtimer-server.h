// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_HRTIMER_DRIVERS_AML_HRTIMER_AML_HRTIMER_SERVER_H_
#define SRC_DEVICES_HRTIMER_DRIVERS_AML_HRTIMER_AML_HRTIMER_SERVER_H_

#include <fidl/fuchsia.hardware.hrtimer/cpp/fidl.h>
#include <lib/async/cpp/irq.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/interrupt.h>

#include <optional>

namespace hrtimer {

class AmlHrtimerServer : public fidl::Server<fuchsia_hardware_hrtimer::Device> {
 public:
  AmlHrtimerServer(async_dispatcher_t* dispatcher, fdf::MmioBuffer mmio, zx::interrupt irq_a,
                   zx::interrupt irq_b, zx::interrupt irq_c, zx::interrupt irq_d,
                   zx::interrupt irq_f, zx::interrupt irq_g, zx::interrupt irq_h,
                   zx::interrupt irq_i);

  void ShutDown() {}
  static size_t GetNumberOfTimers() { return kNumberOfTimers; }  // For unit testing.

 protected:
  void Start(StartRequest& request, StartCompleter::Sync& completer) override;
  void Stop(StopRequest& request, StopCompleter::Sync& completer) override;
  void GetTicksLeft(GetTicksLeftRequest& request, GetTicksLeftCompleter::Sync& completer) override;
  void SetEvent(SetEventRequest& request, SetEventCompleter::Sync& completer) override;
  void StartAndWait(StartAndWaitRequest& request, StartAndWaitCompleter::Sync& completer) override;
  void GetProperties(GetPropertiesCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_hrtimer::Device> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

 private:
  struct Timer {
    Timer(AmlHrtimerServer& server) : parent(server) {}

    void HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                   const zx_packet_interrupt_t* interrupt);

    AmlHrtimerServer& parent;
    std::optional<zx::event> event;
    zx::interrupt irq;
    async::IrqMethod<Timer, &Timer::HandleIrq> irq_handler{this};
    uint64_t start_ticks_requested = 0;
  };

  struct TimersProperties {
    uint64_t id;
    bool supports_event;
    bool supports_system_clock;
    bool supports_1usec;
    bool supports_10usecs;
    bool supports_100usecs;
    bool supports_1msec;
    bool supports_64_bits_tick;  // as opposed to 16bits.
    bool always_on_domain;
    bool watchdog;
  };

  static constexpr size_t kNumberOfTimers = 9;

  static size_t TimerIndexFromId(uint64_t id) { return id; }

  TimersProperties timers_properties_[kNumberOfTimers] = {
      // clang-format off
      // id| event|system|   1us|  10us| 100us|   1ms| 64bit| AOdom|   WDT|
      {   0,  true, false,  true,  true,  true,  true, false, false, false },  // A.
      {   1,  true, false,  true,  true,  true,  true, false, false, false },  // B.
      {   2,  true, false,  true,  true,  true,  true, false, false, false },  // C.
      {   3,  true, false,  true,  true,  true,  true, false, false, false },  // D.
      {   4, false,  true,  true,  true,  true, false, true,  false, false },  // E.
      {   5,  true, false,  true,  true,  true,  true, false, false, false },  // F.
      {   6,  true, false,  true,  true,  true,  true, false, false, false },  // G.
      {   7,  true, false,  true,  true,  true,  true, false, false, false },  // H.
      {   8,  true, false,  true,  true,  true,  true, false, false, false },  // I.
      // The timers below are available in the hardware but not supported by this driver.
      // {   9,  true, false, false, false, false, false, false, false, true  },  // WDT 24MHz.
      // {  10,  true, false,  true,  true,  true, false, false,  true, false },  // AO_A.
      // {  11,  true, false,  true,  true,  true, false, false,  true, false },  // AO_B.
      // {  12, false, false,  true,  true,  true, false, false,  true, false },  // AO_C.
      // // There is no AO_D.
      // {  13, false,  true, false, false, false, false,  true,  true, false },  // AO_E.
      // {  14, false,  true, false, false, false, false,  true,  true, false },  // AO_F.
      // {  15, false,  true, false, false, false, false,  true,  true, false },  // AO_G.
      // {  16, true,   true, false, false, false, false, false,  true, true  },  // AO_WDT.
      // clang-format on
  };

  std::array<Timer, kNumberOfTimers> timers_ = {*this, *this, *this, *this, *this,
                                                *this, *this, *this, *this};
  std::optional<fdf::MmioBuffer> mmio_;
  zx::interrupt irq_;
};
}  // namespace hrtimer
#endif  // SRC_DEVICES_HRTIMER_DRIVERS_AML_HRTIMER_AML_HRTIMER_SERVER_H_
