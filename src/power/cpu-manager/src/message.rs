// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::types::{PState, ThermalLoad};
use fuchsia_zircon::sys;

/// Defines the message types and arguments to be used for inter-node communication
#[derive(Debug, PartialEq)]
#[allow(dead_code)]
pub enum Message {
    /// Get the number of CPUs in the system
    GetNumCpus,

    /// Get the current load from each CPU in the system as a vector of values in the range [0.0 -
    /// 1.0]. Load is calculated by dividing the total time a CPU spent not idle during a duration
    /// by the total time elapsed during the same duration, where the duration is defined as the
    /// time since the previous GetCpuLoads call. The returned vector will have NUM_CPUS elements,
    /// where NUM_CPUS is the value returned by the GetNumCpus message.
    GetCpuLoads,

    /// Get all performance states for the handler's CPU domain
    GetCpuPerformanceStates,

    // Issues the zx_system_set_performance_info syscall.
    SetCpuPerformanceInfo(Vec<sys::zx_cpu_performance_info_t>),

    /// Get the current performance state
    GetPerformanceState,

    /// Set the new performance state
    /// Arg: a value in the range [0 - x] where x is an upper bound defined in the
    /// dev_control_handler crate. An increasing value indicates a lower performance state.
    SetPerformanceState(u32),

    /// Communicate a new thermal load value for the given sensor
    /// Arg0: a ThermalLoad value which represents the severity of thermal load on the given sensor
    /// Arg1: the topological path which uniquely identifies a specific temperature sensor
    UpdateThermalLoad(ThermalLoad, String),
}

/// Defines the return values for each of the Message types from above
#[derive(Debug)]
#[allow(dead_code)]
pub enum MessageReturn {
    /// Arg: the number of CPUs in the system
    GetNumCpus(u32),

    /// Arg: the current load from each CPU in the system as a vector of values in the range [0.0 -
    /// 1.0]. The returned vector will have NUM_CPUS elements, where NUM_CPUS is the value returned
    /// by the GetNumCpus message.
    GetCpuLoads(Vec<f32>),

    /// Arg: all performance states for the CPU domain seviced by the message handler.
    GetCpuPerformanceStates(Vec<PState>),

    /// There is no arg in this MessageReturn type. It only serves as an ACK.
    SetCpuPerformanceInfo,

    /// Arg: the performance state returned from the node
    GetPerformanceState(u32),

    /// There is no arg in this MessageReturn type. It only serves as an ACK.
    SetPerformanceState,

    /// There is no arg in this MessageReturn type. It only serves as an ACK.
    UpdateThermalLoad,
}

pub type MessageResult = Result<MessageReturn, crate::error::CpuManagerError>;
