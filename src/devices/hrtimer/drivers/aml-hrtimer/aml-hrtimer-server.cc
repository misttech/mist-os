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
    zx::interrupt irq_a, zx::interrupt irq_b, zx::interrupt irq_c, zx::interrupt irq_d,
    zx::interrupt irq_f, zx::interrupt irq_g, zx::interrupt irq_h, zx::interrupt irq_i)
    : element_control_(std::move(element_control)) {
  mmio_.emplace(std::move(mmio));

  timers_[0].irq_handler.set_object(irq_a.get());
  timers_[1].irq_handler.set_object(irq_b.get());
  timers_[2].irq_handler.set_object(irq_c.get());
  timers_[3].irq_handler.set_object(irq_d.get());
  // No IRQ on timer id 4.
  timers_[5].irq_handler.set_object(irq_f.get());
  timers_[6].irq_handler.set_object(irq_g.get());
  timers_[7].irq_handler.set_object(irq_h.get());
  timers_[8].irq_handler.set_object(irq_i.get());

  timers_[0].irq_handler.Begin(dispatcher);
  timers_[1].irq_handler.Begin(dispatcher);
  timers_[2].irq_handler.Begin(dispatcher);
  timers_[3].irq_handler.Begin(dispatcher);
  // No IRQ on timer id 4.
  timers_[5].irq_handler.Begin(dispatcher);
  timers_[6].irq_handler.Begin(dispatcher);
  timers_[7].irq_handler.Begin(dispatcher);
  timers_[8].irq_handler.Begin(dispatcher);

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
  if (event) {
    event->signal(0, ZX_EVENT_SIGNALED);
  }

  if (irq.is_valid()) {
    irq.ack();
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
      if (timers_[timer_index].start_ticks_requested == 0) {
        ticks = 0;
      } else {
        // Must read lower 32 bits first.
        ticks = IsaTimerE::Get().ReadFrom(&*mmio_).current_count_value();
        ticks += static_cast<uint64_t>(IsaTimerEHi::Get().ReadFrom(&*mmio_).current_count_value())
                 << 32;
        if (timers_[timer_index].start_ticks_requested > ticks) {
          ticks = timers_[timer_index].start_ticks_requested - ticks;
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
      timers_[timer_index].start_ticks_requested = 0;
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
  completer.Reply(zx::ok());
}

void AmlHrtimerServer::Start(StartRequest& request, StartCompleter::Sync& completer) {
  size_t timer_index = TimerIndexFromId(request.id());
  if (timer_index >= kNumberOfTimers) {
    FDF_LOG(ERROR, "Invalid timer id: %lu", request.id());
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
    return;
  }

  uint32_t count_32bits = 0;
  if (!timers_properties_[timer_index].supports_64_bits_tick) {
    if (request.ticks() > std::numeric_limits<uint16_t>::max()) {
      FDF_LOG(ERROR, "Invalid ticks range: %lu", request.ticks());
      completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
      return;
    }
    count_32bits = static_cast<uint32_t>(request.ticks());
  }
  timers_[timer_index].start_ticks_requested = request.ticks();
  FDF_LOG(TRACE, "Timer id: %lu ticks: %lu", request.id(),
          timers_[timer_index].start_ticks_requested);

  if (!request.resolution().duration()) {
    FDF_LOG(ERROR, "Invalid resolution, no duration");
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
    return;
  }
  uint32_t input_clock_selection = 0;
  uint64_t nsecs = request.resolution().duration().value();
  if (timers_properties_[timer_index].supports_1usec &&
      timers_properties_[timer_index].supports_10usecs &&
      timers_properties_[timer_index].supports_100usecs &&
      timers_properties_[timer_index].supports_1msec) {
    switch (nsecs) {
        // clang-format off
      case zx::usec(1).to_nsecs():   input_clock_selection = 0; break;
      case zx::usec(10).to_nsecs():  input_clock_selection = 1; break;
      case zx::usec(100).to_nsecs(): input_clock_selection = 2; break;
      case zx::msec(1).to_nsecs():   input_clock_selection = 3; break;
        // clang-format on
      default:
        FDF_LOG(ERROR, "Invalid resolution: %lu nsecs", nsecs);
        completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
        return;
    }
    FDF_LOG(TRACE, "Timer id: %lu resolution: %lu nsecs", request.id(), nsecs);
  } else if (timers_properties_[timer_index].supports_1usec &&
             timers_properties_[timer_index].supports_10usecs &&
             timers_properties_[timer_index].supports_100usecs) {
    switch (nsecs) {
        // clang-format off
          case zx::usec(1).to_nsecs():   input_clock_selection = 1; break;
          case zx::usec(10).to_nsecs():  input_clock_selection = 2; break;
          case zx::usec(100).to_nsecs(): input_clock_selection = 3; break;
        // clang-format on
      default:
        FDF_LOG(ERROR, "Invalid resolution: %lu nsecs", nsecs);
        completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
        return;
    }
    FDF_LOG(TRACE, "Timer id: %lu resolution: %lu nsecs", request.id(), nsecs);
  } else {
    FDF_LOG(ERROR, "Invalid resolution state, unsupported combination");
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInternalError));
    return;
  }

  switch (request.id()) {
    case 0:
      IsaTimerA::Get().ReadFrom(&*mmio_).set_starting_count_value(count_32bits).WriteTo(&*mmio_);
      IsaTimerMux::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERA_EN(true)
          .set_TIMERA_MODE(false)
          .set_TIMERA_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 1:
      IsaTimerB::Get().ReadFrom(&*mmio_).set_starting_count_value(count_32bits).WriteTo(&*mmio_);
      IsaTimerMux::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERB_EN(true)
          .set_TIMERB_MODE(false)
          .set_TIMERB_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 2:
      IsaTimerC::Get().ReadFrom(&*mmio_).set_starting_count_value(count_32bits).WriteTo(&*mmio_);
      IsaTimerMux::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERC_EN(true)
          .set_TIMERC_MODE(false)
          .set_TIMERC_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 3:
      IsaTimerD::Get().ReadFrom(&*mmio_).set_starting_count_value(count_32bits).WriteTo(&*mmio_);
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
      IsaTimerF::Get().ReadFrom(&*mmio_).set_starting_count_value(count_32bits).WriteTo(&*mmio_);
      IsaTimerMux1::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERF_EN(true)
          .set_TIMERF_MODE(false)
          .set_TIMERF_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 6:
      IsaTimerG::Get().ReadFrom(&*mmio_).set_starting_count_value(count_32bits).WriteTo(&*mmio_);
      IsaTimerMux1::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERG_EN(true)
          .set_TIMERG_MODE(false)
          .set_TIMERG_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 7:
      IsaTimerH::Get().ReadFrom(&*mmio_).set_starting_count_value(count_32bits).WriteTo(&*mmio_);
      IsaTimerMux1::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERH_EN(true)
          .set_TIMERH_MODE(false)
          .set_TIMERH_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    case 8:
      IsaTimerI::Get().ReadFrom(&*mmio_).set_starting_count_value(count_32bits).WriteTo(&*mmio_);
      IsaTimerMux1::Get()
          .ReadFrom(&*mmio_)
          .set_TIMERI_EN(true)
          .set_TIMERI_MODE(false)
          .set_TIMERI_input_clock_selection(input_clock_selection)
          .WriteTo(&*mmio_);
      break;
    default:
      FDF_LOG(ERROR, "Invalid internal state timer id: %lu", request.id());
      completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInternalError));
      return;
  }
  completer.Reply(zx::ok());
}

void AmlHrtimerServer::SetEvent(SetEventRequest& request, SetEventCompleter::Sync& completer) {
  size_t timer_index = TimerIndexFromId(request.id());
  if (timer_index >= kNumberOfTimers) {
    FDF_LOG(ERROR, "Invalid timer id: %lu", request.id());
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kInvalidArgs));
    return;
  }
  if (!timers_properties_[timer_index].supports_event) {
    FDF_LOG(ERROR, "Not supported event request for timer id: %lu", request.id());
    completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kNotSupported));
    return;
  }
  timers_[timer_index].event.emplace(std::move(request.event()));
  completer.Reply(zx::ok());
}

void AmlHrtimerServer::StartAndWait(StartAndWaitRequest& request,
                                    StartAndWaitCompleter::Sync& completer) {
  completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kNotSupported));
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
    timer_properties.max_ticks(i.supports_64_bits_tick ? std::numeric_limits<uint64_t>::max()
                                                       : std::numeric_limits<uint16_t>::max());
    timer_properties.supports_event(i.supports_event);
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
