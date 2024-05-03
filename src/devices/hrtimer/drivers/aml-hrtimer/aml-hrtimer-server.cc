// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "src/devices/hrtimer/drivers/aml-hrtimer/aml-hrtimer-server.h"

#include <lib/driver/logging/cpp/structured_logger.h>

#include "src/devices/hrtimer/drivers/aml-hrtimer/aml-hrtimer-regs.h"

namespace hrtimer {

AmlHrtimerServer::AmlHrtimerServer(
    async_dispatcher_t* dispatcher, fdf::MmioBuffer mmio,
    std::optional<fidl::ClientEnd<fuchsia_power_broker::ElementControl>> element_control,
    fidl::SyncClient<fuchsia_power_broker::Lessor> lease, zx::interrupt irq_a, zx::interrupt irq_b,
    zx::interrupt irq_c, zx::interrupt irq_d, zx::interrupt irq_f, zx::interrupt irq_g,
    zx::interrupt irq_h, zx::interrupt irq_i)
    : element_control_(std::move(element_control)) {
  mmio_.emplace(std::move(mmio));
  dispatcher_ = dispatcher;
  wake_handling_lessor_ = std::move(lease);

  timers_[0].irq_handler.set_object(irq_a.get());
  timers_[1].irq_handler.set_object(irq_b.get());
  timers_[2].irq_handler.set_object(irq_c.get());
  timers_[3].irq_handler.set_object(irq_d.get());
  // No IRQ on timer id 4.
  timers_[5].irq_handler.set_object(irq_f.get());
  timers_[6].irq_handler.set_object(irq_g.get());
  timers_[7].irq_handler.set_object(irq_h.get());
  timers_[8].irq_handler.set_object(irq_i.get());

  timers_[0].irq = std::move(irq_a);
  timers_[1].irq = std::move(irq_b);
  timers_[2].irq = std::move(irq_c);
  timers_[3].irq = std::move(irq_d);
  // No IRQ on timer id 4.
  timers_[5].irq = std::move(irq_f);
  timers_[6].irq = std::move(irq_g);
  timers_[7].irq = std::move(irq_h);
  timers_[8].irq = std::move(irq_i);
}

void AmlHrtimerServer::Timer::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq_base,
                                        zx_status_t status,
                                        const zx_packet_interrupt_t* interrupt) {
  // Do not log canceled cases; these happen particularly frequently in certain test cases.
  if (status != ZX_ERR_CANCELED) {
    FDF_LOG(INFO, "Timer IRQ triggered: %s", zx_status_get_string(status));
  }

  if (properties.extend_max_ticks && start_ticks_left > std::numeric_limits<uint16_t>::max()) {
    start_ticks_left -= std::numeric_limits<uint16_t>::max();
    // Log re-triggering since it may wakeup the system.
    FDF_LOG(INFO, "Re-trigger timer ticks left: %lu", start_ticks_left);
    size_t timer_index = TimerIndexFromId(properties.id);
    auto start_result = parent.StartHardware(timer_index);
    if (start_result.is_error()) {
      FDF_LOG(ERROR, "Could not restart the hardware for timer index: %zu", timer_index);
      if (wait_completer) {
        wait_completer->Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kBadState));
        wait_completer.reset();
      }
    }
  } else {
    if (wait_completer) {
      // Before we ack the irq we take a lease to prevent the system from suspending while we
      // notify any clients.
      // The lease is passed to the completer or dropped as we exit this scope which guarantees the
      // waiting client was notified before the system suspended.
      auto result_lease = parent.wake_handling_lessor_->Lease(kWakeHandlingLeaseOn);
      // We don't exit on error conditions since we need to potentially signal an event and ack the
      // irq regardless.
      if (result_lease.is_error()) {
        FDF_LOG(ERROR, "Lease returned error: %s",
                result_lease.error_value().FormatDescription().c_str());
        wait_completer->Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kBadState));
      } else {
        fuchsia_hardware_hrtimer::DeviceStartAndWaitResponse response = {
            {.keep_alive = std::move(result_lease->lease_control())},
        };
        wait_completer->Reply(zx::ok(std::move(response)));
      }
      wait_completer.reset();
    }

    if (event) {
      event->signal(0, ZX_EVENT_SIGNALED);
    }
  }

  if (irq.is_valid()) {
    irq.ack();
  }
}

void AmlHrtimerServer::ShutDown() {
  for (auto& i : timers_properties_) {
    size_t timer_index = TimerIndexFromId(i.id);
    if (timers_[timer_index].irq.is_valid()) {
      timers_[timer_index].irq_handler.Cancel();
    }
    if (timers_[timer_index].wait_completer) {
      timers_[timer_index].wait_completer->Reply(
          zx::error(fuchsia_hardware_hrtimer::DriverError::kCanceled));
      timers_[timer_index].wait_completer.reset();
    }
  }
}

void AmlHrtimerServer::GetTicksLeft(GetTicksLeftRequest& request,
                                    GetTicksLeftCompleter::Sync& completer) {
  size_t timer_index = TimerIndexFromId(request.id());
  if (timer_index >= kNumberOfTimers) {
    FDF_LOG(ERROR, "Invalid timer id: %lu", request.id());
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
    return;
  }
  uint64_t ticks = 0;
  switch (request.id()) {
    case 0:
      ticks = IsaTimerA::Get().ReadFrom(&*mmio_).current_count_value();
      break;
    case 1:
      ticks = IsaTimerB::Get().ReadFrom(&*mmio_).current_count_value();
      break;
    case 2:
      ticks = IsaTimerC::Get().ReadFrom(&*mmio_).current_count_value();
      break;
    case 3:
      ticks = IsaTimerD::Get().ReadFrom(&*mmio_).current_count_value();
      break;
    case 4:
      // We either have not started the timer, or we stopped it.
      if (timers_[timer_index].start_ticks_left == 0) {
        ticks = 0;
      } else {
        // Must read lower 32 bits first.
        ticks = IsaTimerE::Get().ReadFrom(&*mmio_).current_count_value();
        ticks += static_cast<uint64_t>(IsaTimerEHi::Get().ReadFrom(&*mmio_).current_count_value())
                 << 32;
        if (timers_[timer_index].start_ticks_left > ticks) {
          ticks = timers_[timer_index].start_ticks_left - ticks;
        } else {
          ticks = 0;
        }
      }
      break;
    case 5:
      ticks = IsaTimerF::Get().ReadFrom(&*mmio_).current_count_value();
      break;
    case 6:
      ticks = IsaTimerG::Get().ReadFrom(&*mmio_).current_count_value();
      break;
    case 7:
      ticks = IsaTimerH::Get().ReadFrom(&*mmio_).current_count_value();
      break;
    case 8:
      ticks = IsaTimerI::Get().ReadFrom(&*mmio_).current_count_value();
      break;
    default:
      completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
      return;
  }

  if (timers_properties_[timer_index].extend_max_ticks) {
    if (timers_[timer_index].start_ticks_left > std::numeric_limits<uint16_t>::max()) {
      ticks += timers_[timer_index].start_ticks_left - std::numeric_limits<uint16_t>::max();
    }
  }
  completer.Reply(zx::ok(ticks));
}

void AmlHrtimerServer::Stop(StopRequest& request, StopCompleter::Sync& completer) {
  size_t timer_index = TimerIndexFromId(request.id());
  if (timer_index >= kNumberOfTimers) {
    FDF_LOG(ERROR, "Invalid timer id: %lu", request.id());
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
    return;
  }
  switch (request.id()) {
    case 0:
      IsaTimerMux::Get().ReadFrom(&*mmio_).set_TIMERA_EN(false).WriteTo(&*mmio_);
      break;
    case 1:
      IsaTimerMux::Get().ReadFrom(&*mmio_).set_TIMERB_EN(false).WriteTo(&*mmio_);
      break;
    case 2:
      IsaTimerMux::Get().ReadFrom(&*mmio_).set_TIMERC_EN(false).WriteTo(&*mmio_);
      break;
    case 3:
      IsaTimerMux::Get().ReadFrom(&*mmio_).set_TIMERD_EN(false).WriteTo(&*mmio_);
      break;
    case 4:
      // Since there is no way to stop the ticking in the hardware we emulate a stop by clearing
      // the ticks requested in start.
      timers_[timer_index].start_ticks_left = 0;
      break;
    case 5:
      IsaTimerMux1::Get().ReadFrom(&*mmio_).set_TIMERF_EN(false).WriteTo(&*mmio_);
      break;
    case 6:
      IsaTimerMux1::Get().ReadFrom(&*mmio_).set_TIMERG_EN(false).WriteTo(&*mmio_);
      break;
    case 7:
      IsaTimerMux1::Get().ReadFrom(&*mmio_).set_TIMERH_EN(false).WriteTo(&*mmio_);
      break;
    case 8:
      IsaTimerMux1::Get().ReadFrom(&*mmio_).set_TIMERI_EN(false).WriteTo(&*mmio_);
      break;
    default:
      FDF_LOG(ERROR, "Invalid internal stop timer id: %lu", request.id());
      completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInternalError));
      return;
  }
  if (timers_[timer_index].irq.is_valid()) {
    timers_[timer_index].irq_handler.Cancel();
  }
  if (timers_[timer_index].wait_completer) {
    timers_[timer_index].wait_completer->Reply(
        zx::error(fuchsia_hardware_hrtimer::DriverError::kCanceled));
    timers_[timer_index].wait_completer.reset();
  }
  completer.Reply(zx::ok());
}

void AmlHrtimerServer::Start(StartRequest& request, StartCompleter::Sync& completer) {
  size_t timer_index = TimerIndexFromId(request.id());
  if (timer_index >= kNumberOfTimers) {
    FDF_LOG(ERROR, "Invalid timer id: %lu", request.id());
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
    return;
  }
  timers_[timer_index].start_ticks_left = request.ticks();
  if (!request.resolution().duration()) {
    FDF_LOG(ERROR, "Invalid resolution, no duration for timer index: %zu", timer_index);
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
    return;
  }
  timers_[timer_index].resolution_nsecs = request.resolution().duration().value();
  auto start_result = StartHardware(timer_index);
  if (start_result.is_error()) {
    completer.Reply(zx::error(start_result.error_value()));
    return;
  }
  if (timers_[timer_index].irq.is_valid()) {
    zx_status_t status = timers_[timer_index].irq_handler.Begin(dispatcher_);
    if (status == ZX_ERR_ALREADY_EXISTS) {
      FDF_LOG(WARNING, "IRQ handler already started for timer id: %lu", request.id());
    }
  }
  completer.Reply(zx::ok());
}

void AmlHrtimerServer::StartAndWait(StartAndWaitRequest& request,
                                    StartAndWaitCompleter::Sync& completer) {
  size_t timer_index = TimerIndexFromId(request.id());
  if (timer_index >= kNumberOfTimers) {
    FDF_LOG(ERROR, "Invalid timer id: %lu", request.id());
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
    return;
  }
  // Only support wait if we can return a lease.
  if (!element_control_ || !timers_properties_[timer_index].supports_notifications) {
    FDF_LOG(ERROR, "Notifications not supported for timer id: %lu", request.id());
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kNotSupported));
    return;
  }
  if (!timers_[timer_index].irq.is_valid()) {
    FDF_LOG(ERROR, "Invalid IRQ for timer id: %lu", request.id());
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInternalError));
    return;
  }
  if (timers_[timer_index].wait_completer.has_value()) {
    FDF_LOG(ERROR, "Invalid state for wait, already waiting for timer id: %lu", request.id());
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kBadState));
    return;
  }
  timers_[timer_index].start_ticks_left = request.ticks();
  if (!request.resolution().duration()) {
    FDF_LOG(ERROR, "Invalid resolution, no duration for timer index: %zu", timer_index);
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
    return;
  }
  timers_[timer_index].resolution_nsecs = request.resolution().duration().value();
  auto start_result = StartHardware(timer_index);
  if (start_result.is_error()) {
    completer.Reply(zx::error(start_result.error_value()));
    return;
  }
  timers_[timer_index].wait_completer.emplace(completer.ToAsync());
  zx_status_t status = timers_[timer_index].irq_handler.Begin(dispatcher_);
  if (status == ZX_ERR_ALREADY_EXISTS) {
    FDF_LOG(WARNING, "IRQ handler already started for timer id: %lu", request.id());
  }
}

fit::result<const fuchsia_hardware_hrtimer::DriverError> AmlHrtimerServer::StartHardware(
    size_t timer_index) {
  uint32_t count_16bits_max = 0;
  const uint64_t ticks = timers_[timer_index].start_ticks_left;
  switch (timers_properties_[timer_index].max_ticks_support) {
    case MaxTicks::k16Bit:
      if (timers_properties_[timer_index].extend_max_ticks &&
          ticks > std::numeric_limits<uint16_t>::max()) {
        count_16bits_max = std::numeric_limits<uint16_t>::max();
      } else {
        if (ticks > std::numeric_limits<uint16_t>::max()) {
          FDF_LOG(ERROR, "Invalid ticks range: %lu for timer index: %zu", ticks, timer_index);
          return fit::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs);
        }
        count_16bits_max = static_cast<uint32_t>(ticks);
      }
      break;
    case MaxTicks::k64Bit:
      break;
  }
  FDF_LOG(TRACE, "Timer index: %zu ticks: %lu", timer_index, ticks);
  uint32_t input_clock_selection = 0;
  const uint64_t resolution_nsecs = timers_[timer_index].resolution_nsecs;
  if (timers_properties_[timer_index].supports_1usec &&
      timers_properties_[timer_index].supports_10usecs &&
      timers_properties_[timer_index].supports_100usecs &&
      timers_properties_[timer_index].supports_1msec) {
    switch (resolution_nsecs) {
        // clang-format off
      case zx::usec(1).to_nsecs():   input_clock_selection = 0; break;
      case zx::usec(10).to_nsecs():  input_clock_selection = 1; break;
      case zx::usec(100).to_nsecs(): input_clock_selection = 2; break;
      case zx::msec(1).to_nsecs():   input_clock_selection = 3; break;
        // clang-format on
      default:
        FDF_LOG(ERROR, "Invalid resolution: %lu nsecs for timer index: %zu", resolution_nsecs,
                timer_index);
        return fit::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs);
    }
    FDF_LOG(TRACE, "Timer index: %zu resolution: %lu nsecs", timer_index, resolution_nsecs);
  } else if (timers_properties_[timer_index].supports_1usec &&
             timers_properties_[timer_index].supports_10usecs &&
             timers_properties_[timer_index].supports_100usecs) {
    switch (resolution_nsecs) {
        // clang-format off
          case zx::usec(1).to_nsecs():   input_clock_selection = 1; break;
          case zx::usec(10).to_nsecs():  input_clock_selection = 2; break;
          case zx::usec(100).to_nsecs(): input_clock_selection = 3; break;
        // clang-format on
      default:
        FDF_LOG(ERROR, "Invalid resolution: %lu nsecs for timer index: %zu", resolution_nsecs,
                timer_index);
        return fit::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs);
    }
    FDF_LOG(TRACE, "Timer index: %zu resolution: %lu nsecs", timer_index, resolution_nsecs);
  } else {
    FDF_LOG(ERROR, "Invalid resolution state, unsupported combination for timer index: %zu",
            timer_index);
    return fit::error(fuchsia_hardware_hrtimer::DriverError::kInternalError);
  }

  switch (timer_index) {
    case 0:
      IsaTimerA::Get()
          .ReadFrom(&*mmio_)
          .set_starting_count_value(count_16bits_max)
          .WriteTo(&*mmio_);
      IsaTimerMux::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERA_EN(true)
          .set_TIMERA_MODE(false)
          .set_TIMERA_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 1:
      IsaTimerB::Get()
          .ReadFrom(&*mmio_)
          .set_starting_count_value(count_16bits_max)
          .WriteTo(&*mmio_);
      IsaTimerMux::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERB_EN(true)
          .set_TIMERB_MODE(false)
          .set_TIMERB_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 2:
      IsaTimerC::Get()
          .ReadFrom(&*mmio_)
          .set_starting_count_value(count_16bits_max)
          .WriteTo(&*mmio_);
      IsaTimerMux::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERC_EN(true)
          .set_TIMERC_MODE(false)
          .set_TIMERC_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 3:
      IsaTimerD::Get()
          .ReadFrom(&*mmio_)
          .set_starting_count_value(count_16bits_max)
          .WriteTo(&*mmio_);
      IsaTimerMux::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERD_EN(true)
          .set_TIMERD_MODE(false)
          .set_TIMERD_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 4:
      IsaTimerMux::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERE_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      IsaTimerE::Get().ReadFrom(&*mmio_).set_current_count_value(0).WriteTo(&*mmio_);
      break;
    case 5:
      IsaTimerF::Get()
          .ReadFrom(&*mmio_)
          .set_starting_count_value(count_16bits_max)
          .WriteTo(&*mmio_);
      IsaTimerMux1::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERF_EN(true)
          .set_TIMERF_MODE(false)
          .set_TIMERF_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 6:
      IsaTimerG::Get()
          .ReadFrom(&*mmio_)
          .set_starting_count_value(count_16bits_max)
          .WriteTo(&*mmio_);
      IsaTimerMux1::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERG_EN(true)
          .set_TIMERG_MODE(false)
          .set_TIMERG_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 7:
      IsaTimerH::Get()
          .ReadFrom(&*mmio_)
          .set_starting_count_value(count_16bits_max)
          .WriteTo(&*mmio_);
      IsaTimerMux1::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERH_EN(true)
          .set_TIMERH_MODE(false)
          .set_TIMERH_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 8:
      IsaTimerI::Get()
          .ReadFrom(&*mmio_)
          .set_starting_count_value(count_16bits_max)
          .WriteTo(&*mmio_);
      IsaTimerMux1::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERI_EN(true)
          .set_TIMERI_MODE(false)
          .set_TIMERI_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    default:
      FDF_LOG(ERROR, "Invalid internal state for timer index: %zu", timer_index);
      return fit::error(fuchsia_hardware_hrtimer::DriverError::kInternalError);
  }
  return fit::success();
}

void AmlHrtimerServer::SetEvent(SetEventRequest& request, SetEventCompleter::Sync& completer) {
  size_t timer_index = TimerIndexFromId(request.id());
  if (timer_index >= kNumberOfTimers) {
    FDF_LOG(ERROR, "Invalid timer id: %lu", request.id());
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
    return;
  }
  if (!timers_properties_[timer_index].supports_notifications) {
    FDF_LOG(ERROR, "Notifications not supported for timer id: %lu", request.id());
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kNotSupported));
    return;
  }
  timers_[timer_index].event.emplace(std::move(request.event()));
  completer.Reply(zx::ok());
}

void AmlHrtimerServer::GetProperties(GetPropertiesCompleter::Sync& completer) {
  std::vector<fuchsia_hardware_hrtimer::TimerProperties> timers_properties;
  for (auto& i : timers_properties_) {
    fuchsia_hardware_hrtimer::TimerProperties timer_properties;
    timer_properties.id(i.id);

    std::vector<fuchsia_hardware_hrtimer::Resolution> resolutions;
    if (i.supports_1usec) {
      resolutions.emplace_back(
          fuchsia_hardware_hrtimer::Resolution::WithDuration(zx::usec(1).to_nsecs()));
    }
    if (i.supports_10usecs) {
      resolutions.emplace_back(
          fuchsia_hardware_hrtimer::Resolution::WithDuration(zx::usec(10).to_nsecs()));
    }
    if (i.supports_100usecs) {
      resolutions.emplace_back(
          fuchsia_hardware_hrtimer::Resolution::WithDuration(zx::usec(100).to_nsecs()));
    }
    if (i.supports_1msec) {
      resolutions.emplace_back(
          fuchsia_hardware_hrtimer::Resolution::WithDuration(zx::msec(1).to_nsecs()));
    }
    timer_properties.supported_resolutions(std::move(resolutions));
    switch (i.max_ticks_support) {
      case MaxTicks::k16Bit:
        if (i.extend_max_ticks) {
          timer_properties.max_ticks(std::numeric_limits<uint64_t>::max());
        } else {
          timer_properties.max_ticks(std::numeric_limits<uint16_t>::max());
        }
        break;
      case MaxTicks::k64Bit:
        timer_properties.max_ticks(std::numeric_limits<uint64_t>::max());
        break;
    }
    timer_properties.supports_event(i.supports_notifications);
    // Only support wait if we can return a lease in StartAndWait.
    timer_properties.supports_wait(element_control_ && i.supports_notifications);
    timers_properties.emplace_back(std::move(timer_properties));
  }

  fuchsia_hardware_hrtimer::Properties properties = {};
  properties.timers_properties(std::move(timers_properties));
  completer.Reply(std::move(properties));
}

void AmlHrtimerServer::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_hrtimer::Device> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {}

}  // namespace hrtimer
