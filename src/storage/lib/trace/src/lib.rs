// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(feature = "tracing")]
mod fuchsia;
#[cfg(feature = "tracing")]
pub use fuchsia::*;

#[cfg(not(feature = "tracing"))]
mod noop;
#[cfg(not(feature = "tracing"))]
pub use noop::*;

#[cfg(test)]
mod tests {
    use crate::TraceFutureExt;

    #[fuchsia::test]
    fn test_duration() {
        let tace_only_var = 6;
        crate::duration!(c"category", c"name");
        crate::duration!(c"category", c"name", "arg" => 5);
        crate::duration!(c"category", c"name", "arg" => 5, "arg2" => tace_only_var);
    }

    #[fuchsia::test]
    fn test_flow_begin() {
        let tace_only_var = 6;
        let flow_id = 5u64;
        crate::flow_begin!(c"category", c"name", flow_id);
        crate::flow_begin!(c"category", c"name", flow_id, "arg" => 5);
        crate::flow_begin!(c"category", c"name", flow_id, "arg" => 5, "arg2" => tace_only_var);
    }

    #[fuchsia::test]
    fn test_flow_step() {
        let tace_only_var = 6;
        let flow_id = 5u64;
        crate::flow_step!(c"category", c"name", flow_id);
        crate::flow_step!(c"category", c"name", flow_id, "arg" => 5);
        crate::flow_step!(c"category", c"name", flow_id, "arg" => 5, "arg2" => tace_only_var);
    }

    #[fuchsia::test]
    fn test_flow_end() {
        let tace_only_var = 6;
        let flow_id = 5u64;
        crate::flow_end!(c"category", c"name", flow_id);
        crate::flow_end!(c"category", c"name", flow_id, "arg" => 5);
        crate::flow_end!(c"category", c"name", flow_id, "arg" => 5, "arg2" => tace_only_var);
    }

    #[fuchsia::test]
    async fn test_trace_future() {
        let value = async move { 5 }.trace(crate::trace_future_args!(c"category", c"name")).await;
        assert_eq!(value, 5);

        let value = async move { 5 }
            .trace(crate::trace_future_args!(c"category", c"name", "arg1" => 6))
            .await;
        assert_eq!(value, 5);

        let tace_only_var = 7;
        let value = async move { 5 }
            .trace(
                crate::trace_future_args!(c"category", c"name", "arg1" => 6, "ar2" => tace_only_var),
            )
            .await;
        assert_eq!(value, 5);
    }
}
