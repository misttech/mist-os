// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <kernel/scheduler.h>
#include <kernel/scheduler_state.h>
#include <ktl/algorithm.h>

void SchedulerState::RecomputeEffectiveProfile() {
  effective_profile_.AssertDirty();

  EffectiveProfile& ep = effective_profile_;
  const BaseProfile& bp = base_profile_;
  const InheritedProfileValues& ipv = inherited_profile_values_;

  if (bp.IsDeadline()) {
    const SchedUtilization total_util = ipv.uncapped_utilization + bp.deadline.utilization;
    const SchedUtilization new_util = ktl::min(total_util, Scheduler::kCpuUtilizationLimit);
    const SchedDuration new_deadline = ktl::min(ipv.min_deadline, bp.deadline.deadline_ns);

    ep.discipline = SchedDiscipline::Deadline;
    ep.deadline = SchedDeadlineParams{new_util, new_deadline};
  } else if (ipv.uncapped_utilization > SchedUtilization{0}) {
    const SchedUtilization new_util =
        ktl::min(ipv.uncapped_utilization, Scheduler::kCpuUtilizationLimit);

    ep.discipline = SchedDiscipline::Deadline;
    ep.deadline = SchedDeadlineParams{new_util, ipv.min_deadline};
  } else {
    // Our thread is fair.  We simply end up inheriting the total weight of the
    // threads blocked behind us.
    ep.discipline = SchedDiscipline::Fair;
    ep.fair.weight = bp.fair.weight + ipv.total_weight;
  }

  effective_profile_.Clean();
}
