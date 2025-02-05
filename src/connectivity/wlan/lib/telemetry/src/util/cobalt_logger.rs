// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Macro wrapper for logging simple events (occurrence, integer, histogram, string)
/// and log a warning when the status is not Ok.
// TODO(339221340): remove these allows once the skeleton has a few uses
#[allow(unused)]
macro_rules! log_cobalt_1dot1 {
    ($cobalt_proxy:expr, $method_name:ident, $metric_id:expr, $value:expr, $event_codes:expr $(,)?) => {{
        let status = $cobalt_proxy.$method_name($metric_id, $value, $event_codes).await;
        match status {
            Ok(Ok(())) => (),
            Ok(Err(e)) => log::info!("Failed logging metric: {}, error: {:?}", $metric_id, e),
            Err(e) => log::info!("Failed logging metric: {}, error: {}", $metric_id, e),
        }
    }};
}

macro_rules! log_cobalt_1dot1_batch {
    ($cobalt_proxy:expr, $events:expr, $context:expr $(,)?) => {{
        let status = $cobalt_proxy.log_metric_events($events).await;
        match status {
            Ok(Ok(())) => (),
            Ok(Err(e)) => {
                log::info!("Failed logging batch metrics, context: {}, error: {:?}", $context, e);
            }
            Err(e) => {
                log::info!("Failed logging batch metrics, context: {}, error: {}", $context, e)
            }
        }
    }};
}

// TODO(339221340): remove these allows once the skeleton has a few uses
#[allow(unused)]
pub(crate) use {log_cobalt_1dot1, log_cobalt_1dot1_batch};
