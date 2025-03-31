// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2008-2014 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

/**
 * @defgroup debug  Debug
 * @{
 */

/**
 * @file
 * @brief  Debug console functions.
 */

#include <debug.h>
#include <inttypes.h>
#include <lib/concurrent/copy.h>
#include <lib/console.h>
#include <lib/kconcurrent/chainlock_transaction.h>
#include <lib/zircon-internal/macros.h>
#include <platform.h>
#include <stdio.h>
#include <string.h>
#include <zircon/errors.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <ffl/string.h>
#include <kernel/cpu.h>
#include <kernel/mp.h>
#include <kernel/percpu.h>
#include <kernel/scheduler.h>
#include <kernel/thread.h>
#include <vm/vm.h>

static int cmd_thread(int argc, const cmd_args* argv, uint32_t flags);
static int cmd_threadstats(int argc, const cmd_args* argv, uint32_t flags);
static int cmd_threadload(int argc, const cmd_args* argv, uint32_t flags);
static int cmd_threadq(int argc, const cmd_args* argv, uint32_t flags);
static int cmd_zmips(int argc, const cmd_args* argv, uint32_t flags);
static int cmd_rppm(int argc, const cmd_args* argv, uint32_t flags);

STATIC_COMMAND_START
STATIC_COMMAND_MASKED("thread", "manipulate kernel threads", &cmd_thread, CMD_AVAIL_ALWAYS)
STATIC_COMMAND("threadstats", "thread level statistics", &cmd_threadstats)
STATIC_COMMAND("threadload", "toggle thread load display", &cmd_threadload)
STATIC_COMMAND("threadq", "toggle thread queue display", &cmd_threadq)
STATIC_COMMAND("zmips", "compute zmips of a cpu", &cmd_zmips)
STATIC_COMMAND_MASKED("rppm", "runtime processor power management commands", &cmd_rppm,
                      CMD_AVAIL_ALWAYS)
STATIC_COMMAND_END(kernel)

static int cmd_thread(int argc, const cmd_args* argv, uint32_t flags) {
  if (argc < 2) {
  notenoughargs:
    printf("not enough arguments\n");
  usage:
    printf("%s bt <thread pointer or id>\n", argv[0].str);
    printf("%s dump <thread pointer or id>\n", argv[0].str);
    printf("%s list\n", argv[0].str);
    printf("%s list_full\n", argv[0].str);
    return -1;
  }

  if (!strcmp(argv[1].str, "bt")) {
    if (argc < 3) {
      goto notenoughargs;
    }

    {
      // Hold the list lock so that thread objects cannot destruct while we dump
      // this info.
      //
      // Note that the practice of dumping info about a thread by direct kernel
      // address is inherently dangerous.  No validation of the pointer is
      // currently done.
      Guard<SpinLock, IrqSave> list_lock_guard(&Thread::get_list_lock());
      Thread* t = NULL;
      if (is_kernel_address(argv[2].u)) {
        t = (Thread*)argv[2].u;
      } else {
        t = thread_id_to_thread_slow(argv[2].u);
      }
      if (t) {
        Backtrace bt;
        t->GetBacktrace(bt);
        bt.Print();
      }
    }
  } else if (!strcmp(argv[1].str, "dump")) {
    if (argc < 3) {
      goto notenoughargs;
    }

    Thread* t = NULL;
    if (is_kernel_address(argv[2].u)) {
      t = (Thread*)argv[2].u;
      t->Dump(true);
    } else {
      if (flags & CMD_FLAG_PANIC) {
        Thread::DumpTidDuringPanic(argv[2].u, true);
      } else {
        Thread::DumpTid(argv[2].u, true);
      }
    }
  } else if (!strcmp(argv[1].str, "list")) {
    printf("thread list:\n");
    if (flags & CMD_FLAG_PANIC) {
      Thread::DumpAllDuringPanic(false);
    } else {
      Thread::DumpAll(false);
    }
  } else if (!strcmp(argv[1].str, "list_full")) {
    printf("thread list:\n");
    if (flags & CMD_FLAG_PANIC) {
      Thread::DumpAllDuringPanic(true);
    } else {
      Thread::DumpAll(true);
    }
  } else {
    printf("invalid args\n");
    goto usage;
  }

  // reschedule to let debuglog potentially run
  if (!(flags & CMD_FLAG_PANIC)) {
    Thread::Current::Reschedule();
  }

  return 0;
}

static int cmd_threadstats(int argc, const cmd_args* argv, uint32_t flags) {
  for (cpu_num_t i = 0; i < percpu::processor_count(); i++) {
    if (!Scheduler::PeekIsActive(i)) {
      continue;
    }
    const auto& percpu = percpu::Get(i);

    printf("thread stats (cpu %u):\n", i);
    printf("\ttotal idle time: %" PRIi64 "\n", percpu.stats.idle_time);
    printf("\ttotal busy time: %" PRIi64 "\n",
           zx_time_sub_duration(current_mono_time(), percpu.stats.idle_time));
    printf("\treschedules: %lu\n", percpu.stats.reschedules);
    printf("\treschedule_ipis: %lu\n", percpu.stats.reschedule_ipis);
    printf("\tcontext_switches: %lu\n", percpu.stats.context_switches);
    printf("\tpreempts: %lu\n", percpu.stats.preempts);
    printf("\tyields: %lu\n", percpu.stats.yields);
    printf("\ttimer interrupts: %lu\n", percpu.stats.timer_ints);
    printf("\ttimers: %lu\n", percpu.stats.timers);
  }

  return 0;
}

namespace {

RecurringCallback g_threadload_callback([]() {
  static struct cpu_stats old_stats[SMP_MAX_CPUS];
  static zx_duration_mono_t last_idle_time[SMP_MAX_CPUS]{0};

  printf(
      "cpu    load"
      " sched (cs ylds pmpts irq_pmpts)"
      "   sysc"
      " ints (hw  tmr tmr_cb)"
      " ipi (rs  gen)"
      " pwr (active   idle)\n");
  for (cpu_num_t i = 0; i < percpu::processor_count(); i++) {
    const Thread& idle_power_thread = percpu::Get(i).idle_power_thread.thread();
    struct cpu_stats stats;

    SingleChainLockGuard thread_guard{IrqSaveOption, idle_power_thread.get_lock(),
                                      CLT_TAG("g_threadload_callback")};
    using optional_duration = ktl::optional<zx_duration_mono_t>;
    auto maybe_idle_time = Scheduler::RunInLockedScheduler(i, [&]() -> optional_duration {
      // dont display time for inactive cpus
      if (!Scheduler::PeekIsActive(i)) {
        return ktl::nullopt;
      }

      {
        const auto& percpu = percpu::Get(i);
        concurrent::WellDefinedCopyFrom<concurrent::SyncOpt::None, alignof(decltype(stats))>(
            &stats, &percpu.stats, sizeof(stats));
      }

      // if the cpu is currently idle, add the time since it went idle up until now to the idle
      // counter
      if (Scheduler::PeekIsIdle(i)) {
        ChainLockTransaction::AssertActive();
        idle_power_thread.get_lock().AssertHeld();
        zx_duration_mono_t recent_idle_time = zx_time_sub_time(
            current_mono_time(), idle_power_thread.scheduler_state().last_started_running());
        return zx_duration_add_duration(stats.idle_time, recent_idle_time);
      } else {
        return stats.idle_time;
      }
    });

    if (!maybe_idle_time.has_value()) {
      continue;
    }

    const zx_duration_mono_t idle_time = maybe_idle_time.value();
    const zx_duration_mono_t delta_time = zx_duration_sub_duration(idle_time, last_idle_time[i]);
    const zx_duration_mono_t busy_time =
        (ZX_SEC(1) > delta_time) ? zx_duration_sub_duration(ZX_SEC(1), delta_time) : 0;
    zx_duration_mono_t busypercent = zx_duration_mul_int64(busy_time, 10000) / ZX_SEC(1);

    const uint64_t kNanosToMillis = 1'000'000u;
    const uint64_t kRoundNanosToMillis = 500'000u;

    const uint64_t active_energy_delta_nj =
        stats.active_energy_consumption_nj - old_stats[i].active_energy_consumption_nj;
    const uint64_t active_power_mw =
        (active_energy_delta_nj + kRoundNanosToMillis) / kNanosToMillis / ZX_SEC(1);

    const uint64_t idle_energy_delta_nj =
        stats.idle_energy_consumption_nj - old_stats[i].idle_energy_consumption_nj;
    const uint64_t idle_power_mw =
        (idle_energy_delta_nj + kRoundNanosToMillis) / kNanosToMillis / ZX_SEC(1);

    printf(
        "%3u"
        " %3u.%02u%%"
        " %9lu %4lu %5lu %9lu"
        " %7lu"
        " %8lu %4lu %6lu"
        " %8lu %4lu"
        " %8u.%03u %2u.%03u"
        "\n",
        i, static_cast<uint>(busypercent / 100), static_cast<uint>(busypercent % 100),
        stats.context_switches - old_stats[i].context_switches, stats.yields - old_stats[i].yields,
        stats.preempts - old_stats[i].preempts, stats.irq_preempts - old_stats[i].irq_preempts,
        stats.syscalls - old_stats[i].syscalls, stats.interrupts - old_stats[i].interrupts,
        stats.timer_ints - old_stats[i].timer_ints, stats.timers - old_stats[i].timers,
        stats.reschedule_ipis - old_stats[i].reschedule_ipis,
        stats.generic_ipis - old_stats[i].generic_ipis, static_cast<uint>(active_power_mw / 1000),
        static_cast<uint>(active_power_mw % 1000), static_cast<uint>(idle_power_mw / 1000),
        static_cast<uint>(idle_power_mw % 1000));

    old_stats[i] = stats;
    last_idle_time[i] = idle_time;
  }
});

RecurringCallback g_threadq_callback([]() {
  printf("----------------------------------------------------\n");
  for (cpu_num_t i = 0; i < percpu::processor_count(); i++) {
    if (Scheduler::PeekIsActive(i)) {
      printf("thread queue cpu %2u:\n", i);
      percpu::Get(i).scheduler.Dump();
    }
  }
  printf("\n");
});

}  // anonymous namespace
static int cmd_threadload(int argc, const cmd_args* argv, uint32_t flags) {
  g_threadload_callback.Toggle();
  return 0;
}

static int cmd_threadq(int argc, const cmd_args* argv, uint32_t flags) {
  g_threadq_callback.Toggle();
  return 0;
}

static int cmd_zmips(int argc, const cmd_args* argv, uint32_t flags) {
  if (argc < 2) {
    printf("Not enough arguments.\n");
  usage:
    printf("%s <cpu number>\n", argv[0].str);
    return -1;
  }

  cpu_num_t cpu_num = static_cast<cpu_num_t>(argv[1].u);
  if (cpu_num >= percpu::processor_count()) {
    printf("CPU number must be in the range [%zu, %zu].\n", size_t{0},
           percpu::processor_count() - 1);
    goto usage;
  }

  const auto calibrate = [](void* arg) -> int {
    const cpu_num_t cpu_num = *static_cast<cpu_num_t*>(arg);

    // Busy loop for the given number of iterations.
    const auto delay = [](uint64_t loops) {
      while (loops != 0) {
        __asm__ volatile("" : "+g"(loops)::);
        --loops;
      }
    };

    using U30 = ffl::Fixed<uint64_t, 30>;
    U30 zmips_min = U30::Max();
    U30 zmips_max = U30::Min();

    const int max_samples = 10;
    const uint64_t max_loops = uint64_t{1} << 48;
    const zx_duration_mono_t target_duration_ns = ZX_SEC(1) / 20;

    for (int i = 0; i < max_samples; i++) {
      // Quickly find the number of loops it takes for the delay loop to run for at least the target
      // duration by stepping in power of two increments, avoiding excessively large values.
      for (uint64_t loops = 1; loops < max_loops; loops *= 2) {
        // Disable interrupts to limit the noise of the measurement. The target duration is selected
        // to provide suitable precision without disabling interrupts for too long to risk tripping
        // software/hardware watchdogs.
        InterruptDisableGuard interrupt_disable;
        const zx_instant_mono_t start_ns = current_mono_time();
        delay(loops);
        const zx_instant_mono_t stop_ns = current_mono_time();
        interrupt_disable.Reenable();

        const zx_duration_mono_t duration_ns = zx_time_sub_time(stop_ns, start_ns);
        if (duration_ns >= target_duration_ns) {
          printf("Calibrating CPU %u: %" PRIu64 " loops per %" PRId64 " ns\n", cpu_num, loops,
                 duration_ns);

          // On simpler, in-order architectures the number of loops corresponds to half of the clock
          // rate. Scale this value by two for minor convenience.
          const U30 zmips = 2 * U30{loops} / duration_ns * 1000;
          zmips_min = ktl::min(zmips_min, zmips);
          zmips_max = ktl::max(zmips_max, zmips);
          break;
        }
      }
    }

    printf("Calibrated CPU %u: %s-%s ZMIPS\n", cpu_num,
           Format(zmips_min, ffl::String::Mode::Dec, 2).c_str(),
           Format(zmips_max, ffl::String::Mode::Dec, 2).c_str());

    return 0;
  };

  Thread* thread = Thread::Create("calibrate_zmips", +calibrate, &cpu_num, DEFAULT_PRIORITY);
  if (thread == nullptr) {
    printf("Failed to create calibration thread!\n");
    return -1;
  }

  thread->SetCpuAffinity(cpu_num_to_mask(cpu_num));
  thread->Resume();

  int retcode;
  thread->Join(&retcode, ZX_TIME_INFINITE);

  return retcode;
}

static int cmd_rppm(int argc, const cmd_args* argv, uint32_t flags) {
  if (argc < 2) {
  notenoughargs:
    printf("not enough arguments\n");
  badvalue:
    printf("%s dump\n", argv[0].str);
    printf("%s set-level <cpu id> <power level>\n", argv[0].str);
    printf("%s req-level <cpu id> <power level>\n", argv[0].str);
    printf("%s get-level <cpu id>\n", argv[0].str);
    return -1;
  }

  if (!strcmp(argv[1].str, "dump")) {
    percpu::ForEach([](cpu_num_t cpu, percpu* percpu) {
      Scheduler& scheduler = percpu->scheduler;

      fbl::RefPtr domain = scheduler.GetPowerDomainForTesting();
      ktl::optional<uint8_t> current_power_level = scheduler.GetPowerLevel();
      ktl::optional<uint64_t> active_power_coefficient_nw =
          scheduler.GetActivePowerCoefficientNwForTesting();
      ktl::optional<uint64_t> max_idle_power_coefficient_nw =
          scheduler.GetMaxIdlePowerCoefficientNwForTesting();

      printf("CPU %u:\n", cpu);

      if (domain) {
        printf("  Power domain %u:\n", domain->id());
        for (size_t index = 0; const auto& power_level : domain->model().levels()) {
          printf("    %3zu. rate=%" PRIu64 " power=%" PRIu64 " nw control=%s\n", index++,
                 power_level.processing_rate(), power_level.power_coefficient_nw(),
                 ToString(power_level.control()));
        }
      } else {
        printf("  No power domain set.\n");
      }

      if (current_power_level.has_value()) {
        printf("  Current power level %u.\n", *current_power_level);
      } else {
        printf("  Current power level not set.\n");
      }

      if (active_power_coefficient_nw.has_value()) {
        printf("  Active power coefficient %" PRIu64 " nw\n", *active_power_coefficient_nw);
      } else {
        printf("  Active power coefficient not set.\n");
      }

      if (max_idle_power_coefficient_nw.has_value()) {
        printf("  Max idle power coefficient %" PRIu64 " nw\n", *max_idle_power_coefficient_nw);
      } else {
        printf("  Max idle power coefficient not set.\n");
      }
    });
    return 0;
  }

  if (const bool is_set_level = !strcmp(argv[1].str, "set-level"),
      is_req_level = !strcmp(argv[1].str, "req-level"),
      is_get_level = !strcmp(argv[1].str, "get-level");
      is_set_level || is_req_level || is_get_level) {
    if (((is_set_level || is_req_level) && argc < 4) || (is_get_level && argc < 3)) {
      goto notenoughargs;
    }

    if (argv[2].u >= percpu::processor_count()) {
      printf("Invalid cpu id %" PRIu64 ". Valid values are in the range [0, %zu].\n", argv[2].u,
             percpu::processor_count() - 1);
      goto badvalue;
    }
    const cpu_num_t cpu = static_cast<cpu_num_t>(argv[2].u);

    Scheduler& scheduler = percpu::Get(cpu).scheduler;

    if (is_set_level || is_req_level) {
      if (argv[3].u >= std::numeric_limits<uint8_t>::max()) {
        printf("Invalid power level\n");
        goto badvalue;
      }
      const uint8_t power_level = static_cast<uint8_t>(argv[3].u);

      if (is_set_level) {
        const zx::result result = scheduler.SetPowerLevel(power_level);
        if (result.is_error()) {
          printf("Failed to set power level: %d\n", result.status_value());
        } else {
          printf("Set CPU %u to power level %u\n", cpu, power_level);
        }
      } else if (is_req_level) {
        scheduler.RequestPowerLevelForTesting(power_level);
        printf("CPU %u request for power level %u posted.\n", cpu, power_level);
      }
    } else if (is_get_level) {
      ktl::optional<uint8_t> maybe_power_level = scheduler.GetPowerLevel();
      if (maybe_power_level.has_value()) {
        printf("CPU %u at power level %u\n", cpu, maybe_power_level.value());
      } else {
        printf("CPU %u power level not set\n", cpu);
      }
    }
  }

  return 0;
}
