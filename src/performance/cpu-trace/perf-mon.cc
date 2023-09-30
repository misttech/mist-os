// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// See the README.md in this directory for documentation.

#include "perf-mon.h"

#include <assert.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zircon-internal/device/cpu-trace/perf-mon.h>
#include <lib/zircon-internal/mtrace.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <cstdint>
#include <limits>
#include <memory>
#include <utility>

namespace perfmon {

int ComparePerfmonEventId(const void* ap, const void* bp) {
  auto a = reinterpret_cast<const EventId*>(ap);
  auto b = reinterpret_cast<const EventId*>(bp);
  if (*a < *b) {
    return -1;
  }
  if (*a > *b) {
    return 1;
  }
  return 0;
}

uint16_t GetLargestEventId(const EventDetails* events, size_t count) {
  uint16_t largest = 0;

  for (size_t i = 0; i < count; ++i) {
    uint16_t id = events[i].id;
    if (id > largest) {
      largest = id;
    }
  }

  return largest;
}

zx_status_t BuildEventMap(const EventDetails* events, uint16_t count,
                          const uint16_t** out_event_map, size_t* out_map_size) {
  static_assert(kMaxEvent < std::numeric_limits<uint16_t>::max());

  uint16_t largest_event_id = GetLargestEventId(events, count);
  // See perf-mon.h: The full event id is split into two pieces:
  // group type and id within that group. The event recorded in
  // |EventDetails| is the id within the group. Each id must be in
  // the range [1,PERFMON_MAX_EVENT]. ID 0 is reserved.
  if (largest_event_id == 0 || largest_event_id > kMaxEvent) {
    FX_LOGS(ERROR) << "PMU: Corrupt event database";
    return ZX_ERR_INTERNAL;
  }

  size_t event_map_size = largest_event_id + 1;
  FX_LOGS(DEBUG) << "PMU: " << count << " arch events";
  FX_LOGS(DEBUG) << "PMU: arch event id range: 1-" << event_map_size;
  auto event_map = new uint16_t[event_map_size]{0};

  for (uint16_t i = 0; i < count; ++i) {
    uint16_t id = events[i].id;
    assert(id < event_map_size);
    assert(event_map[id] == 0);
    event_map[id] = i;
  }

  *out_event_map = event_map;
  FX_LOGS(INFO) << "Setting event_map_size: " << event_map_size;
  *out_map_size = event_map_size;
  return ZX_OK;
}

static void DumpHwProperties(const PmuHwProperties& props) {
  FX_LOGS(INFO) << "Performance Monitor Unit configuration for this chipset:";
  FX_LOGS(INFO) << "PMU: version " << props.common.pm_version;
  FX_LOGS(INFO) << "PMU: " << props.common.max_num_fixed_events << " fixed events, width "
                << props.common.max_fixed_counter_width;
  FX_LOGS(INFO) << "PMU: " << props.common.max_num_programmable_events
                << " programmable events, width " << props.common.max_programmable_counter_width;
  FX_LOGS(INFO) << "PMU: " << props.common.max_num_misc_events << " misc events, width "
                << props.common.max_misc_counter_width;
#ifdef __x86_64__
  FX_LOGS(INFO) << "PMU: perf_capabilities: 0x" << std::hex << props.perf_capabilities << std::dec;
  FX_LOGS(INFO) << "PMU: lbr_stack_size: " << props.lbr_stack_size;
#endif
}

PerfmonController::PerfmonController(perfmon::PmuHwProperties props,
                                     mtrace_control_func_t* mtrace_control,
                                     zx::unowned_resource debug_resource)
    : pmu_hw_properties_(props),
      mtrace_control_(mtrace_control),
      debug_resource_(std::move(std::move(debug_resource))) {
  DumpHwProperties(props);
}

zx_status_t PerfmonController::GetHwProperties(mtrace_control_func_t* mtrace_control,
                                               PmuHwProperties* out_props,
                                               const zx::unowned_resource& debug_resource) {
  zx_status_t status =
      mtrace_control(debug_resource->get(), MTRACE_KIND_PERFMON, MTRACE_PERFMON_GET_PROPERTIES, 0,
                     out_props, sizeof(*out_props));

  if (status == ZX_ERR_NOT_SUPPORTED) {
    FX_PLOGS(WARNING, status) << "No PM support";
  } else if (status != ZX_OK) {
    FX_PLOGS(WARNING, status) << "Could not fetch perfmon properties";
  }

  return status;
}

void PerfmonController::FreeBuffersForTrace(PmuPerTraceState* per_trace, uint32_t num_allocated) {
  // Note: This may be called with partially allocated buffers.
  assert(num_allocated <= per_trace->num_buffers);
  for (const auto& [mapping, size] : per_trace->mappings) {
    zx::vmar::root_self()->unmap(mapping, size);
  }
  per_trace->buffers.clear();
}

void PerfmonController::PmuGetProperties(FidlPerfmonProperties* props) const {
  props->api_version = fidl_perfmon::wire::kApiVersion;
  props->pm_version = pmu_hw_properties_.common.pm_version;
  static_assert(perfmon::kMaxNumEvents == fidl_perfmon::wire::kMaxNumEvents);
  props->max_num_events = fidl_perfmon::wire::kMaxNumEvents;

  // These numbers are for informational/debug purposes. There can be
  // further restrictions and limitations.
  // TODO(dje): Something more elaborate can wait for publishing them via
  // some namespace.
  props->max_num_fixed_events = pmu_hw_properties_.common.max_num_fixed_events;
  props->max_fixed_counter_width = pmu_hw_properties_.common.max_fixed_counter_width;
  props->max_num_programmable_events = pmu_hw_properties_.common.max_num_programmable_events;
  props->max_programmable_counter_width = pmu_hw_properties_.common.max_programmable_counter_width;
  props->max_num_misc_events = pmu_hw_properties_.common.max_num_misc_events;
  props->max_misc_counter_width = pmu_hw_properties_.common.max_misc_counter_width;

  props->flags = fidl_perfmon::wire::PropertyFlags();
#ifdef __x86_64__
  if (pmu_hw_properties_.lbr_stack_size > 0) {
    props->flags |= fidl_perfmon::wire::PropertyFlags::kHasLastBranch;
  }
#endif
}

zx_status_t PerfmonController::PmuInitialize(const FidlPerfmonAllocation* allocation) {
  if (per_trace_state_) {
    return ZX_ERR_BAD_STATE;
  }

  uint32_t num_cpus = zx_system_get_num_cpus();
  if (allocation->num_buffers != num_cpus) {  // TODO(dje): for now
    return ZX_ERR_INVALID_ARGS;
  }
  if (allocation->buffer_size_in_pages > kMaxPerTraceSpaceInPages) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto per_trace = std::make_unique<PmuPerTraceState>();
  per_trace->buffers = std::vector<zx::vmo>{num_cpus};

  size_t buffer_size = static_cast<size_t>(allocation->buffer_size_in_pages) * kPageSize;

  for (uint32_t i = 0; i < num_cpus; ++i) {
    zx_status_t status = zx::vmo::create(buffer_size, 0, &per_trace->buffers[i]);
    if (status != ZX_OK) {
      FreeBuffersForTrace(per_trace.get(), i);
      return status;
    }

    zx_vm_option_t map_options = ZX_VM_PERM_READ;

    zx_vaddr_t virt;
    status =
        zx::vmar::root_self()->map(map_options, 0, per_trace->buffers[i], 0, buffer_size, &virt);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "zx_vmar_map failed";
      return status;
    }
    per_trace->mappings.emplace_back(virt, buffer_size);
  }

  per_trace->num_buffers = allocation->num_buffers;
  per_trace->buffer_size_in_pages = allocation->buffer_size_in_pages;
  per_trace_state_ = std::move(per_trace);
  return ZX_OK;
}

void PerfmonController::PmuTerminate() {
  if (active_) {
    PmuStop();
  }

  PmuPerTraceState* per_trace = per_trace_state_.get();
  if (per_trace) {
    FreeBuffersForTrace(per_trace, per_trace->num_buffers);
    per_trace_state_.reset();
  }
}

zx_status_t PerfmonController::PmuGetAllocation(FidlPerfmonAllocation* allocation) {
  const PmuPerTraceState* per_trace = per_trace_state_.get();
  if (!per_trace) {
    return ZX_ERR_BAD_STATE;
  }

  allocation->num_buffers = per_trace->num_buffers;
  allocation->buffer_size_in_pages = per_trace->buffer_size_in_pages;
  return ZX_OK;
}

zx_status_t PerfmonController::PmuGetBufferHandle(uint32_t descriptor, zx_handle_t* out_handle) {
  const PmuPerTraceState* per_trace = per_trace_state_.get();
  if (!per_trace) {
    return ZX_ERR_BAD_STATE;
  }

  if (descriptor >= per_trace->num_buffers) {
    return ZX_ERR_INVALID_ARGS;
  }

  zx::vmo h;
  zx_status_t status = per_trace->buffers[descriptor].duplicate(ZX_RIGHT_SAME_RIGHTS, &h);
  if (status != ZX_OK) {
    // This failure could be hard to debug. Give the user some help.
    FX_PLOGS(ERROR, status) << "Failed to duplicate " << descriptor << " buffer handle";
    return status;
  }

  *out_handle = h.release();
  return ZX_OK;
}

// Do an architecture-independent verification pass over |icfg|,
// and see if there's a timebase event.
static zx_status_t VerifyAndCheckTimebase(const FidlPerfmonConfig* icfg, PmuConfig* ocfg) {
  unsigned ii;  // ii: input index
  for (ii = 0; ii < std::size(icfg->events); ++ii) {
    EventId id = icfg->events[ii].event;
    if (id == kEventIdNone) {
      break;
    }
    EventRate rate = icfg->events[ii].rate;
    fidl_perfmon::wire::EventConfigFlags flags = icfg->events[ii].flags;

    if (flags & fidl_perfmon::wire::EventConfigFlags::kIsTimebase) {
      if (ocfg->timebase_event != kEventIdNone) {
        FX_LOGS(ERROR) << "multiple timebases [" << ii << "]";
        return ZX_ERR_INVALID_ARGS;
      }
      ocfg->timebase_event = icfg->events[ii].event;
    }

    if (flags & fidl_perfmon::wire::EventConfigFlags::kCollectPc) {
      if (rate == 0) {
        FX_LOGS(ERROR) << "PC flag requires own timebase, event [" << ii << "]";
        return ZX_ERR_INVALID_ARGS;
      }
    }

    if (flags & fidl_perfmon::wire::EventConfigFlags::kCollectLastBranch) {
      // Further verification is architecture specific.
      if (icfg->events[ii].rate == 0) {
        FX_LOGS(ERROR) << "Last branch requires own timebase, event [" << ii << "]";
        return ZX_ERR_INVALID_ARGS;
      }
    }
  }

  if (ii == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Ensure there are no holes.
  for (; ii < std::size(icfg->events); ++ii) {
    if (icfg->events[ii].event != kEventIdNone) {
      FX_LOGS(ERROR) << "Hole at event [" << ii << "]";
      return ZX_ERR_INVALID_ARGS;
    }
    if (icfg->events[ii].rate != 0) {
      FX_LOGS(ERROR) << "Hole at rate [" << ii << "]";
      return ZX_ERR_INVALID_ARGS;
    }
    if (icfg->events[ii].flags != fidl_perfmon::wire::EventConfigFlags()) {
      FX_LOGS(ERROR) << "Hole at flags [" << ii << "]";
      return ZX_ERR_INVALID_ARGS;
    }
  }

  return ZX_OK;
}

zx_status_t PerfmonController::PmuStageConfig(const FidlPerfmonConfig* fidl_config) {
  if (active_) {
    return ZX_ERR_BAD_STATE;
  }
  PmuPerTraceState* per_trace = per_trace_state_.get();
  if (!per_trace) {
    return ZX_ERR_BAD_STATE;
  }

  // If we subsequently get an error, make sure any previous configuration
  // can't be used.
  per_trace->configured = false;

  const FidlPerfmonConfig* icfg = fidl_config;

  PmuConfig* ocfg = &per_trace->config;
  *ocfg = {};

  // Validate the config and convert it to our internal form.
  // TODO(dje): Multiplexing support.

  StagingState staging_state{};
  StagingState* ss = &staging_state;
  InitializeStagingState(ss);

  zx_status_t status = VerifyAndCheckTimebase(icfg, ocfg);
  if (status != ZX_OK) {
    return status;
  }

  for (size_t ii = 0; ii < decltype(icfg->events)::size(); ++ii) {
    EventId id = icfg->events[ii].event;
    FX_LOGS(DEBUG) << "processing [" << ii << "] = " << id;
    if (id == kEventIdNone) {
      break;
    }
    unsigned group = GetEventIdGroup(id);

    switch (group) {
      case kGroupFixed:
        status = StageFixedConfig(icfg, ss, ii, ocfg);
        if (status != ZX_OK) {
          return status;
        }
        break;
      case kGroupArch:
      case kGroupModel:
        status = StageProgrammableConfig(icfg, ss, ii, ocfg);
        if (status != ZX_OK) {
          return status;
        }
        break;
      case kGroupMisc:
        status = StageMiscConfig(icfg, ss, ii, ocfg);
        if (status != ZX_OK) {
          return status;
        }
        break;
      default:
        FX_LOGS(ERROR) << "Invalid event [" << ii << "] (bad group)";
        return ZX_ERR_INVALID_ARGS;
    }
  }

  // TODO(dje): Basic validity check that some data will be collected.

  per_trace->fidl_config = *icfg;
  per_trace->configured = true;
  return ZX_OK;
}

zx_status_t PerfmonController::PmuGetConfig(FidlPerfmonConfig* config) {
  const PmuPerTraceState* per_trace = per_trace_state_.get();
  if (!per_trace) {
    return ZX_ERR_BAD_STATE;
  }

  if (!per_trace->configured) {
    return ZX_ERR_BAD_STATE;
  }

  *config = per_trace->fidl_config;
  return ZX_OK;
}

zx_status_t PerfmonController::PmuStart() {
  if (active_) {
    return ZX_ERR_BAD_STATE;
  }
  PmuPerTraceState* per_trace = per_trace_state_.get();
  if (!per_trace) {
    return ZX_ERR_BAD_STATE;
  }

  if (!per_trace->configured) {
    return ZX_ERR_BAD_STATE;
  }

  // Step 1: Get the configuration data into the kernel for use by START.

#ifdef __x86_64__
  FX_LOGS(DEBUG) << std::hex << "global ctrl 0x" << per_trace->config.global_ctrl
                 << ", fixed ctrl 0x" << per_trace->config.fixed_ctrl;
  // Note: If only misc counters are enabled then |global_ctrl| will be zero.
#endif

  zx_status_t status = mtrace_control_(debug_resource_->get(), MTRACE_KIND_PERFMON,
                                       MTRACE_PERFMON_INIT, 0, nullptr, 0);
  if (status != ZX_OK) {
    return status;
  }

  uint32_t num_cpus = zx_system_get_num_cpus();
  for (uint32_t cpu = 0; cpu < num_cpus; ++cpu) {
    zx_pmu_buffer_t buffer{.vmo = per_trace->buffers[cpu].get()};
    status = mtrace_control_(debug_resource_->get(), MTRACE_KIND_PERFMON,
                             MTRACE_PERFMON_ASSIGN_BUFFER, cpu, &buffer, sizeof(buffer));
    if (status != ZX_OK) {
      goto fail;
    }
  }

  status = mtrace_control_(debug_resource_->get(), MTRACE_KIND_PERFMON, MTRACE_PERFMON_STAGE_CONFIG,
                           0, &per_trace->config, sizeof(per_trace->config));
  if (status != ZX_OK) {
    goto fail;
  }

  // Step 2: Start data collection.

  status = mtrace_control_(debug_resource_->get(), MTRACE_KIND_PERFMON, MTRACE_PERFMON_START, 0,
                           nullptr, 0);
  if (status != ZX_OK) {
    goto fail;
  }

  active_ = true;
  return ZX_OK;

fail: {
  [[maybe_unused]] zx_status_t status2 = mtrace_control_(
      debug_resource_->get(), MTRACE_KIND_PERFMON, MTRACE_PERFMON_FINI, 0, nullptr, 0);
  assert(status2 == ZX_OK);
  return status;
}
}

void PerfmonController::PmuStop() {
  PmuPerTraceState* per_trace = per_trace_state_.get();
  if (!per_trace) {
    return;
  }

  [[maybe_unused]] zx_status_t status = mtrace_control_(debug_resource_->get(), MTRACE_KIND_PERFMON,
                                                        MTRACE_PERFMON_STOP, 0, nullptr, 0);
  assert(status == ZX_OK);
  active_ = false;
  status = mtrace_control_(debug_resource_->get(), MTRACE_KIND_PERFMON, MTRACE_PERFMON_FINI, 0,
                           nullptr, 0);
  assert(status == ZX_OK);
}

// Fidl interface.

void PerfmonController::GetProperties(GetPropertiesCompleter::Sync& completer) {
  FidlPerfmonProperties props{};
  PmuGetProperties(&props);
  completer.Reply(props);
}

void PerfmonController::Initialize(InitializeRequestView request,
                                   InitializeCompleter::Sync& completer) {
  zx_status_t status = PmuInitialize(&request->allocation);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void PerfmonController::Terminate(TerminateCompleter::Sync& completer) {
  PmuTerminate();
  completer.Reply();
}

void PerfmonController::GetAllocation(GetAllocationCompleter::Sync& completer) {
  FidlPerfmonAllocation alloc{};
  zx_status_t status = PmuGetAllocation(&alloc);
  completer.Reply(status != ZX_OK ? nullptr
                                  : fidl::ObjectView<FidlPerfmonAllocation>::FromExternal(&alloc));
}

void PerfmonController::StageConfig(StageConfigRequestView request,
                                    StageConfigCompleter::Sync& completer) {
  zx_status_t status = PmuStageConfig(&request->config);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void PerfmonController::GetConfig(GetConfigCompleter::Sync& completer) {
  FidlPerfmonConfig config{};
  zx_status_t status = PmuGetConfig(&config);
  completer.Reply(status != ZX_OK ? nullptr
                                  : fidl::ObjectView<FidlPerfmonConfig>::FromExternal(&config));
}

void PerfmonController::GetBufferHandle(GetBufferHandleRequestView request,
                                        GetBufferHandleCompleter::Sync& completer) {
  zx_handle_t handle;
  zx_status_t status = PmuGetBufferHandle(request->descriptor, &handle);
  completer.Reply(zx::vmo(status != ZX_OK ? ZX_HANDLE_INVALID : handle));
}

void PerfmonController::Start(StartCompleter::Sync& completer) {
  zx_status_t status = PmuStart();
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void PerfmonController::Stop(StopCompleter::Sync& completer) {
  PmuStop();
  completer.Reply();
}
}  // namespace perfmon
