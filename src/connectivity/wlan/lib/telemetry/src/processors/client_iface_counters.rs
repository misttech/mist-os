// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::util::cobalt_logger::log_cobalt_1dot1_batch;
use fidl_fuchsia_metrics::{MetricEvent, MetricEventPayload};
use fidl_fuchsia_wlan_stats as fidl_stats;
use fuchsia_async::{self as fasync, TimeoutExt};
use futures::lock::Mutex;

use log::{error, warn};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use windowed_stats::experimental::clock::Timed;
use windowed_stats::experimental::series::interpolation::{Constant, LastSample};
use windowed_stats::experimental::series::statistic::{
    ArithmeticMean, Last, LatchMax, Max, Min, Sum,
};
use windowed_stats::experimental::series::{SamplingProfile, TimeMatrix};
use windowed_stats::experimental::serve::{InspectSender, InspectedTimeMatrix};
use wlan_legacy_metrics_registry as metrics;

// Include a timeout on stats calls so that if the driver deadlocks, telemtry doesn't get stuck.
const GET_IFACE_STATS_TIMEOUT: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(5);

#[derive(Debug)]
enum IfaceState {
    NotAvailable,
    Created { iface_id: u16, telemetry_proxy: Option<fidl_fuchsia_wlan_sme::TelemetryProxy> },
}

type CountersTimeSeriesMap = HashMap<u16, InspectedTimeMatrix<u64>>;
type GaugesTimeSeriesMap = HashMap<u16, Vec<InspectedTimeMatrix<i64>>>;

pub struct ClientIfaceCountersLogger<S> {
    iface_state: Arc<Mutex<IfaceState>>,
    cobalt_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
    monitor_svc_proxy: fidl_fuchsia_wlan_device_service::DeviceMonitorProxy,
    time_series_stats: IfaceCountersTimeSeries,
    driver_counters_time_matrix_client: S,
    driver_counters_time_series: Arc<Mutex<CountersTimeSeriesMap>>,
    driver_gauges_time_matrix_client: S,
    driver_gauges_time_series: Arc<Mutex<GaugesTimeSeriesMap>>,
    prev_connection_stats: Arc<Mutex<Option<fidl_stats::ConnectionStats>>>,
    boot_mono_drift: AtomicI64,
}

impl<S: InspectSender> ClientIfaceCountersLogger<S> {
    pub fn new(
        cobalt_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
        monitor_svc_proxy: fidl_fuchsia_wlan_device_service::DeviceMonitorProxy,
        time_matrix_client: &S,
        driver_counters_time_matrix_client: S,
        driver_gauges_time_matrix_client: S,
    ) -> Self {
        Self {
            iface_state: Arc::new(Mutex::new(IfaceState::NotAvailable)),
            cobalt_proxy,
            monitor_svc_proxy,
            time_series_stats: IfaceCountersTimeSeries::new(time_matrix_client),
            driver_counters_time_matrix_client,
            driver_counters_time_series: Arc::new(Mutex::new(HashMap::new())),
            driver_gauges_time_matrix_client,
            driver_gauges_time_series: Arc::new(Mutex::new(HashMap::new())),
            prev_connection_stats: Arc::new(Mutex::new(None)),
            boot_mono_drift: AtomicI64::new(0),
        }
    }

    pub async fn handle_iface_created(&self, iface_id: u16) {
        let (proxy, server) = fidl::endpoints::create_proxy();
        let telemetry_proxy = match self.monitor_svc_proxy.get_sme_telemetry(iface_id, server).await
        {
            Ok(Ok(())) => {
                let (inspect_counter_configs, inspect_gauge_configs) = match proxy
                    .query_telemetry_support()
                    .await
                {
                    Ok(Ok(support)) => {
                        (support.inspect_counter_configs, support.inspect_gauge_configs)
                    }
                    Ok(Err(code)) => {
                        warn!("Failed to query telemetry support with status code {}. No driver-specific stats will be captured", code);
                        (None, None)
                    }
                    Err(e) => {
                        error!("Failed to query telemetry support with error {}. No driver-specific stats will be captured", e);
                        (None, None)
                    }
                };
                if let Some(inspect_counter_configs) = &inspect_counter_configs {
                    let mut driver_counters_time_series =
                        self.driver_counters_time_series.lock().await;
                    for inspect_counter_config in inspect_counter_configs {
                        if let fidl_stats::InspectCounterConfig {
                            counter_id: Some(counter_id),
                            counter_name: Some(counter_name),
                            ..
                        } = inspect_counter_config
                        {
                            let _time_matrix_ref = driver_counters_time_series
                                .entry(*counter_id)
                                .or_insert_with(|| {
                                    self.driver_counters_time_matrix_client.inspect_time_matrix(
                                        counter_name,
                                        TimeMatrix::<LatchMax<u64>, LastSample>::new(
                                            SamplingProfile::balanced(),
                                            LastSample::or(0),
                                        ),
                                    )
                                });
                        }
                    }
                }
                if let Some(inspect_gauge_configs) = &inspect_gauge_configs {
                    let mut driver_gauges_time_series = self.driver_gauges_time_series.lock().await;
                    for inspect_gauge_config in inspect_gauge_configs {
                        if let fidl_stats::InspectGaugeConfig {
                            gauge_id: Some(gauge_id),
                            gauge_name: Some(gauge_name),
                            statistics: Some(statistics),
                            ..
                        } = inspect_gauge_config
                        {
                            for statistic in statistics {
                                if let Some(time_matrix) = create_time_series_for_gauge(
                                    &self.driver_gauges_time_matrix_client,
                                    gauge_name,
                                    statistic,
                                ) {
                                    let time_matrices =
                                        driver_gauges_time_series.entry(*gauge_id).or_default();
                                    time_matrices.push(time_matrix);
                                }
                            }
                        }
                    }
                }
                Some(proxy)
            }
            Ok(Err(e)) => {
                error!("Request for SME telemetry for iface {} completed with error {}. No telemetry will be captured.", iface_id, e);
                None
            }
            Err(e) => {
                error!("Failed to request SME telemetry for iface {} with error {}. No telemetry will be captured.", iface_id, e);
                None
            }
        };
        *self.iface_state.lock().await = IfaceState::Created { iface_id, telemetry_proxy }
    }

    pub async fn handle_iface_destroyed(&self, iface_id: u16) {
        let destroyed = matches!(*self.iface_state.lock().await, IfaceState::Created { iface_id: existing_iface_id, .. } if iface_id == existing_iface_id);
        if destroyed {
            *self.iface_state.lock().await = IfaceState::NotAvailable;
        }
    }

    pub async fn handle_periodic_telemetry(&self) {
        let boot_now = fasync::BootInstant::now();
        let mono_now = fasync::MonotonicInstant::now();
        let boot_mono_drift = boot_now.into_nanos() - mono_now.into_nanos();
        let prev_boot_mono_drift = self.boot_mono_drift.swap(boot_mono_drift, Ordering::SeqCst);
        // If the difference between boot time and monotonic time has increased, it means that
        // there was a suspension since the last time `handle_periodic_telemetry` was called.
        let suspended_during_last_period = boot_mono_drift > prev_boot_mono_drift;
        match &*self.iface_state.lock().await {
            IfaceState::NotAvailable => (),
            IfaceState::Created { telemetry_proxy, .. } => {
                if let Some(telemetry_proxy) = &telemetry_proxy {
                    match telemetry_proxy
                        .get_iface_stats()
                        .on_timeout(GET_IFACE_STATS_TIMEOUT, || {
                            Ok(Err(zx::Status::TIMED_OUT.into_raw()))
                        })
                        .await
                    {
                        Ok(Ok(stats)) => {
                            self.log_iface_stats_inspect(&stats).await;
                            self.log_iface_stats_cobalt(stats, suspended_during_last_period).await;
                        }
                        error => {
                            warn!("Failed to get interface stats: {:?}", error);
                        }
                    }
                }
            }
        }
    }

    async fn log_iface_stats_inspect(&self, stats: &fidl_stats::IfaceStats) {
        // Iface-level driver specific counters
        if let Some(counters) = &stats.driver_specific_counters {
            let time_series = Arc::clone(&self.driver_counters_time_series);
            log_driver_specific_counters(&counters[..], time_series).await;
        }
        // Iface-level driver specific gauges
        if let Some(gauges) = &stats.driver_specific_gauges {
            let time_series = Arc::clone(&self.driver_gauges_time_series);
            log_driver_specific_gauges(&gauges[..], time_series).await;
        }
        log_connection_stats_inspect(
            stats,
            &self.time_series_stats,
            Arc::clone(&self.driver_counters_time_series),
            Arc::clone(&self.driver_gauges_time_series),
        )
        .await;
    }

    async fn log_iface_stats_cobalt(
        &self,
        stats: fidl_stats::IfaceStats,
        suspended_during_last_period: bool,
    ) {
        let mut prev_connection_stats = self.prev_connection_stats.lock().await;
        // Only log to Cobalt if there was no suspension in-between
        if !suspended_during_last_period {
            if let (Some(prev_connection_stats), Some(current_connection_stats)) =
                (prev_connection_stats.as_ref(), stats.connection_stats.as_ref())
            {
                match (prev_connection_stats.connection_id, current_connection_stats.connection_id)
                {
                    (Some(prev_id), Some(current_id)) if prev_id == current_id => {
                        diff_and_log_connection_stats_cobalt(
                            &self.cobalt_proxy,
                            prev_connection_stats,
                            current_connection_stats,
                        )
                        .await;
                    }
                    _ => (),
                }
            }
        }
        *prev_connection_stats = stats.connection_stats;
    }
}

fn create_time_series_for_gauge<S: InspectSender>(
    time_matrix_client: &S,
    gauge_name: &str,
    statistic: &fidl_stats::GaugeStatistic,
) -> Option<InspectedTimeMatrix<i64>> {
    match statistic {
        fidl_stats::GaugeStatistic::Min => Some(time_matrix_client.inspect_time_matrix(
            format!("{gauge_name}.min"),
            TimeMatrix::<Min<i64>, Constant>::new(SamplingProfile::balanced(), Constant::default()),
        )),
        fidl_stats::GaugeStatistic::Max => Some(time_matrix_client.inspect_time_matrix(
            format!("{gauge_name}.max"),
            TimeMatrix::<Max<i64>, Constant>::new(SamplingProfile::balanced(), Constant::default()),
        )),
        fidl_stats::GaugeStatistic::Sum => Some(time_matrix_client.inspect_time_matrix(
            format!("{gauge_name}.sum"),
            TimeMatrix::<Sum<i64>, Constant>::new(SamplingProfile::balanced(), Constant::default()),
        )),
        fidl_stats::GaugeStatistic::Last => Some(time_matrix_client.inspect_time_matrix(
            format!("{gauge_name}.last"),
            TimeMatrix::<Last<i64>, Constant>::new(
                SamplingProfile::balanced(),
                Constant::default(),
            ),
        )),
        fidl_stats::GaugeStatistic::Mean => Some(time_matrix_client.inspect_time_matrix(
            format!("{gauge_name}.mean"),
            TimeMatrix::<ArithmeticMean<i64>, Constant>::new(
                SamplingProfile::balanced(),
                Constant::default(),
            ),
        )),
        _ => None,
    }
}

async fn log_connection_stats_inspect(
    stats: &fidl_stats::IfaceStats,
    time_series_stats: &IfaceCountersTimeSeries,
    driver_counters_time_series: Arc<Mutex<CountersTimeSeriesMap>>,
    driver_gauges_time_series: Arc<Mutex<GaugesTimeSeriesMap>>,
) {
    let connection_stats = match &stats.connection_stats {
        Some(counters) => counters,
        None => return,
    };

    // Enforce that `connection_id` field is there for us to log driver counters.
    match &connection_stats.connection_id {
        Some(_connection_id) => (),
        _ => {
            warn!("connection_id is not present, no connection counters will be logged");
            return;
        }
    }

    if let fidl_stats::ConnectionStats {
        rx_unicast_total: Some(rx_unicast_total),
        rx_unicast_drop: Some(rx_unicast_drop),
        ..
    } = connection_stats
    {
        time_series_stats.log_rx_unicast_total(*rx_unicast_total);
        time_series_stats.log_rx_unicast_drop(*rx_unicast_drop);
    }

    if let fidl_stats::ConnectionStats {
        tx_total: Some(tx_total), tx_drop: Some(tx_drop), ..
    } = connection_stats
    {
        time_series_stats.log_tx_total(*tx_total);
        time_series_stats.log_tx_drop(*tx_drop);
    }

    // Connection-level driver-specific counters
    if let Some(counters) = &connection_stats.driver_specific_counters {
        log_driver_specific_counters(&counters[..], driver_counters_time_series).await;
    }
    // Connection-level driver-specific gauges
    if let Some(gauges) = &connection_stats.driver_specific_gauges {
        log_driver_specific_gauges(&gauges[..], driver_gauges_time_series).await;
    }
}

async fn log_driver_specific_counters(
    driver_specific_counters: &[fidl_stats::UnnamedCounter],
    driver_counters_time_series: Arc<Mutex<CountersTimeSeriesMap>>,
) {
    let time_series_map = driver_counters_time_series.lock().await;
    for counter in driver_specific_counters {
        if let Some(ts) = time_series_map.get(&counter.id) {
            ts.fold_or_log_error(Timed::now(counter.count));
        }
    }
}

async fn log_driver_specific_gauges(
    driver_specific_gauges: &[fidl_stats::UnnamedGauge],
    driver_gauges_time_series: Arc<Mutex<GaugesTimeSeriesMap>>,
) {
    let time_series_map = driver_gauges_time_series.lock().await;
    for gauge in driver_specific_gauges {
        if let Some(time_matrices) = time_series_map.get(&gauge.id) {
            for ts in time_matrices {
                ts.fold_or_log_error(Timed::now(gauge.value));
            }
        }
    }
}

async fn diff_and_log_connection_stats_cobalt(
    cobalt_proxy: &fidl_fuchsia_metrics::MetricEventLoggerProxy,
    prev: &fidl_stats::ConnectionStats,
    current: &fidl_stats::ConnectionStats,
) {
    diff_and_log_rx_cobalt(cobalt_proxy, prev, current).await;
    diff_and_log_tx_cobalt(cobalt_proxy, prev, current).await;
}

async fn diff_and_log_rx_cobalt(
    cobalt_proxy: &fidl_fuchsia_metrics::MetricEventLoggerProxy,
    prev: &fidl_stats::ConnectionStats,
    current: &fidl_stats::ConnectionStats,
) {
    let mut metric_events = vec![];

    let (current_rx_unicast_total, prev_rx_unicast_total) =
        match (current.rx_unicast_total, prev.rx_unicast_total) {
            (Some(current), Some(prev)) => (current, prev),
            _ => return,
        };
    let (current_rx_unicast_drop, prev_rx_unicast_drop) =
        match (current.rx_unicast_drop, prev.rx_unicast_drop) {
            (Some(current), Some(prev)) => (current, prev),
            _ => return,
        };

    let rx_total = current_rx_unicast_total - prev_rx_unicast_total;
    let rx_drop = current_rx_unicast_drop - prev_rx_unicast_drop;
    let rx_drop_rate = if rx_total > 0 { rx_drop as f64 / rx_total as f64 } else { 0f64 };

    metric_events.push(MetricEvent {
        metric_id: metrics::BAD_RX_RATE_METRIC_ID,
        event_codes: vec![],
        payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(rx_drop_rate)),
    });
    metric_events.push(MetricEvent {
        metric_id: metrics::RX_UNICAST_PACKETS_METRIC_ID,
        event_codes: vec![],
        payload: MetricEventPayload::IntegerValue(rx_total as i64),
    });

    log_cobalt_1dot1_batch!(cobalt_proxy, &metric_events, "diff_and_log_rx_cobalt",);
}

async fn diff_and_log_tx_cobalt(
    cobalt_proxy: &fidl_fuchsia_metrics::MetricEventLoggerProxy,
    prev: &fidl_stats::ConnectionStats,
    current: &fidl_stats::ConnectionStats,
) {
    let mut metric_events = vec![];

    let (current_tx_total, prev_tx_total) = match (current.tx_total, prev.tx_total) {
        (Some(current), Some(prev)) => (current, prev),
        _ => return,
    };
    let (current_tx_drop, prev_tx_drop) = match (current.tx_drop, prev.tx_drop) {
        (Some(current), Some(prev)) => (current, prev),
        _ => return,
    };

    let tx_total = current_tx_total - prev_tx_total;
    let tx_drop = current_tx_drop - prev_tx_drop;
    let tx_drop_rate = if tx_total > 0 { tx_drop as f64 / tx_total as f64 } else { 0f64 };

    metric_events.push(MetricEvent {
        metric_id: metrics::BAD_TX_RATE_METRIC_ID,
        event_codes: vec![],
        payload: MetricEventPayload::IntegerValue(float_to_ten_thousandth(tx_drop_rate)),
    });

    log_cobalt_1dot1_batch!(cobalt_proxy, &metric_events, "diff_and_log_tx_cobalt",);
}

// Convert float to an integer in "ten thousandth" unit
// Example: 0.02f64 (i.e. 2%) -> 200 per ten thousand
fn float_to_ten_thousandth(value: f64) -> i64 {
    (value * 10000f64) as i64
}

#[derive(Debug, Clone)]
struct IfaceCountersTimeSeries {
    rx_unicast_total: InspectedTimeMatrix<u64>,
    rx_unicast_drop: InspectedTimeMatrix<u64>,
    tx_total: InspectedTimeMatrix<u64>,
    tx_drop: InspectedTimeMatrix<u64>,
}

impl IfaceCountersTimeSeries {
    pub fn new<S: InspectSender>(client: &S) -> Self {
        let rx_unicast_total = client.inspect_time_matrix(
            "rx_unicast_total",
            TimeMatrix::<LatchMax<u64>, LastSample>::new(
                SamplingProfile::balanced(),
                LastSample::or(0),
            ),
        );
        let rx_unicast_drop = client.inspect_time_matrix(
            "rx_unicast_drop",
            TimeMatrix::<LatchMax<u64>, LastSample>::new(
                SamplingProfile::balanced(),
                LastSample::or(0),
            ),
        );
        let tx_total = client.inspect_time_matrix(
            "tx_total",
            TimeMatrix::<LatchMax<u64>, LastSample>::new(
                SamplingProfile::balanced(),
                LastSample::or(0),
            ),
        );
        let tx_drop = client.inspect_time_matrix(
            "tx_drop",
            TimeMatrix::<LatchMax<u64>, LastSample>::new(
                SamplingProfile::balanced(),
                LastSample::or(0),
            ),
        );
        Self { rx_unicast_total, rx_unicast_drop, tx_total, tx_drop }
    }

    fn log_rx_unicast_total(&self, data: u64) {
        self.rx_unicast_total.fold_or_log_error(Timed::now(data));
    }

    fn log_rx_unicast_drop(&self, data: u64) {
        self.rx_unicast_drop.fold_or_log_error(Timed::now(data));
    }

    fn log_tx_total(&self, data: u64) {
        self.tx_total.fold_or_log_error(Timed::now(data));
    }

    fn log_tx_drop(&self, data: u64) {
        self.tx_drop.fold_or_log_error(Timed::now(data));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use futures::TryStreamExt;
    use std::pin::pin;
    use std::task::Poll;
    use test_case::test_case;
    use windowed_stats::experimental::testing::{MockTimeMatrixClient, TimeMatrixCall};
    use wlan_common::assert_variant;

    const IFACE_ID: u16 = 66;

    #[fuchsia::test]
    fn test_handle_iface_created() {
        let mut test_helper = setup_test();
        let driver_counters_mock_matrix_client = MockTimeMatrixClient::new();
        let driver_gauges_mock_matrix_client = MockTimeMatrixClient::new();
        let logger = ClientIfaceCountersLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            test_helper.monitor_svc_proxy.clone(),
            &test_helper.mock_time_matrix_client,
            driver_counters_mock_matrix_client.clone(),
            driver_gauges_mock_matrix_client.clone(),
        );

        let mut handle_iface_created_fut = pin!(logger.handle_iface_created(IFACE_ID));
        assert_eq!(
            test_helper.run_and_handle_get_sme_telemetry(&mut handle_iface_created_fut),
            Poll::Pending
        );

        let mocked_inspect_counter_configs = vec![fidl_stats::InspectCounterConfig {
            counter_id: Some(1),
            counter_name: Some("foo_counter".to_string()),
            ..Default::default()
        }];
        let telemetry_support = fidl_stats::TelemetrySupport {
            inspect_counter_configs: Some(mocked_inspect_counter_configs),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_query_telemetry_support(
                &mut handle_iface_created_fut,
                Ok(&telemetry_support)
            ),
            Poll::Ready(())
        );

        assert_variant!(logger.iface_state.try_lock().as_deref(), Some(IfaceState::Created { .. }));
        let driver_counters_time_series = logger.driver_counters_time_series.try_lock().unwrap();
        assert_eq!(driver_counters_time_series.keys().copied().collect::<Vec<u16>>(), vec![1u16],);
    }

    #[fuchsia::test]
    fn test_handle_periodic_telemetry_connection_stats() {
        let mut test_helper = setup_test();
        let driver_counters_mock_matrix_client = MockTimeMatrixClient::new();
        let driver_gauges_mock_matrix_client = MockTimeMatrixClient::new();
        let logger = ClientIfaceCountersLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            test_helper.monitor_svc_proxy.clone(),
            &test_helper.mock_time_matrix_client,
            driver_counters_mock_matrix_client.clone(),
            driver_gauges_mock_matrix_client.clone(),
        );

        // Transition to IfaceCreated state
        handle_iface_created(&mut test_helper, &logger);

        let mut test_fut = pin!(logger.handle_periodic_telemetry());
        let iface_stats = fidl_stats::IfaceStats {
            connection_stats: Some(fidl_stats::ConnectionStats {
                connection_id: Some(1),
                rx_unicast_total: Some(100),
                rx_unicast_drop: Some(5),
                rx_multicast: Some(30),
                tx_total: Some(50),
                tx_drop: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_iface_stats_req(&mut test_fut, Ok(&iface_stats)),
            Poll::Ready(())
        );

        let mut time_matrix_calls = test_helper.mock_time_matrix_client.drain_calls();
        assert_eq!(
            &time_matrix_calls.drain::<u64>("rx_unicast_total")[..],
            &[TimeMatrixCall::Fold(Timed::now(100u64))]
        );
        assert_eq!(
            &time_matrix_calls.drain::<u64>("rx_unicast_drop")[..],
            &[TimeMatrixCall::Fold(Timed::now(5u64))]
        );
        assert_eq!(
            &time_matrix_calls.drain::<u64>("tx_total")[..],
            &[TimeMatrixCall::Fold(Timed::now(50u64))]
        );
        assert_eq!(
            &time_matrix_calls.drain::<u64>("tx_drop")[..],
            &[TimeMatrixCall::Fold(Timed::now(2u64))]
        );
    }

    #[fuchsia::test]
    fn test_handle_periodic_telemetry_driver_specific_counters() {
        let mut test_helper = setup_test();
        let driver_counters_mock_matrix_client = MockTimeMatrixClient::new();
        let driver_gauges_mock_matrix_client = MockTimeMatrixClient::new();
        let logger = ClientIfaceCountersLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            test_helper.monitor_svc_proxy.clone(),
            &test_helper.mock_time_matrix_client,
            driver_counters_mock_matrix_client.clone(),
            driver_gauges_mock_matrix_client.clone(),
        );

        let mut handle_iface_created_fut = pin!(logger.handle_iface_created(IFACE_ID));
        assert_eq!(
            test_helper.run_and_handle_get_sme_telemetry(&mut handle_iface_created_fut),
            Poll::Pending
        );

        let mocked_inspect_configs = vec![
            fidl_stats::InspectCounterConfig {
                counter_id: Some(1),
                counter_name: Some("foo_counter".to_string()),
                ..Default::default()
            },
            fidl_stats::InspectCounterConfig {
                counter_id: Some(2),
                counter_name: Some("bar_counter".to_string()),
                ..Default::default()
            },
            fidl_stats::InspectCounterConfig {
                counter_id: Some(3),
                counter_name: Some("baz_counter".to_string()),
                ..Default::default()
            },
        ];
        let telemetry_support = fidl_stats::TelemetrySupport {
            inspect_counter_configs: Some(mocked_inspect_configs),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_query_telemetry_support(
                &mut handle_iface_created_fut,
                Ok(&telemetry_support)
            ),
            Poll::Ready(())
        );

        let mut test_fut = pin!(logger.handle_periodic_telemetry());
        let iface_stats = fidl_stats::IfaceStats {
            driver_specific_counters: Some(vec![fidl_stats::UnnamedCounter { id: 1, count: 50 }]),
            connection_stats: Some(fidl_stats::ConnectionStats {
                connection_id: Some(1),
                driver_specific_counters: Some(vec![
                    fidl_stats::UnnamedCounter { id: 2, count: 100 },
                    fidl_stats::UnnamedCounter { id: 3, count: 150 },
                    // This one is no-op because it's not registered in QueryTelemetrySupport
                    fidl_stats::UnnamedCounter { id: 4, count: 200 },
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_iface_stats_req(&mut test_fut, Ok(&iface_stats)),
            Poll::Ready(())
        );

        let time_matrix_calls = test_helper.mock_time_matrix_client.drain_calls();
        assert!(time_matrix_calls.is_empty());

        let mut driver_counters_matrix_calls = driver_counters_mock_matrix_client.drain_calls();
        assert_eq!(
            &driver_counters_matrix_calls.drain::<u64>("foo_counter")[..],
            &[TimeMatrixCall::Fold(Timed::now(50))]
        );
        assert_eq!(
            &driver_counters_matrix_calls.drain::<u64>("bar_counter")[..],
            &[TimeMatrixCall::Fold(Timed::now(100))]
        );
        assert_eq!(
            &driver_counters_matrix_calls.drain::<u64>("baz_counter")[..],
            &[TimeMatrixCall::Fold(Timed::now(150))]
        );

        let driver_gauges_matrix_calls = driver_gauges_mock_matrix_client.drain_calls();
        assert!(driver_gauges_matrix_calls.is_empty());
    }

    #[fuchsia::test]
    fn test_handle_periodic_telemetry_driver_specific_gauges() {
        let mut test_helper = setup_test();
        let driver_counters_mock_matrix_client = MockTimeMatrixClient::new();
        let driver_gauges_mock_matrix_client = MockTimeMatrixClient::new();
        let logger = ClientIfaceCountersLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            test_helper.monitor_svc_proxy.clone(),
            &test_helper.mock_time_matrix_client,
            driver_counters_mock_matrix_client.clone(),
            driver_gauges_mock_matrix_client.clone(),
        );

        let mut handle_iface_created_fut = pin!(logger.handle_iface_created(IFACE_ID));
        assert_eq!(
            test_helper.run_and_handle_get_sme_telemetry(&mut handle_iface_created_fut),
            Poll::Pending
        );

        let mocked_inspect_configs = vec![
            fidl_stats::InspectGaugeConfig {
                gauge_id: Some(1),
                gauge_name: Some("foo_gauge".to_string()),
                statistics: Some(vec![
                    fidl_stats::GaugeStatistic::Mean,
                    fidl_stats::GaugeStatistic::Last,
                ]),
                ..Default::default()
            },
            fidl_stats::InspectGaugeConfig {
                gauge_id: Some(2),
                gauge_name: Some("bar_gauge".to_string()),
                statistics: Some(vec![
                    fidl_stats::GaugeStatistic::Min,
                    fidl_stats::GaugeStatistic::Sum,
                ]),
                ..Default::default()
            },
            fidl_stats::InspectGaugeConfig {
                gauge_id: Some(3),
                gauge_name: Some("baz_gauge".to_string()),
                statistics: Some(vec![fidl_stats::GaugeStatistic::Max]),
                ..Default::default()
            },
        ];
        let telemetry_support = fidl_stats::TelemetrySupport {
            inspect_gauge_configs: Some(mocked_inspect_configs),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_query_telemetry_support(
                &mut handle_iface_created_fut,
                Ok(&telemetry_support)
            ),
            Poll::Ready(())
        );

        let mut test_fut = pin!(logger.handle_periodic_telemetry());
        let iface_stats = fidl_stats::IfaceStats {
            driver_specific_gauges: Some(vec![fidl_stats::UnnamedGauge { id: 1, value: 50 }]),
            connection_stats: Some(fidl_stats::ConnectionStats {
                connection_id: Some(1),
                driver_specific_gauges: Some(vec![
                    fidl_stats::UnnamedGauge { id: 2, value: 100 },
                    fidl_stats::UnnamedGauge { id: 3, value: 150 },
                    // This one is no-op because it's not registered in QueryTelemetrySupport
                    fidl_stats::UnnamedGauge { id: 4, value: 200 },
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_iface_stats_req(&mut test_fut, Ok(&iface_stats)),
            Poll::Ready(())
        );

        let time_matrix_calls = test_helper.mock_time_matrix_client.drain_calls();
        assert!(time_matrix_calls.is_empty());

        let driver_counters_matrix_calls = driver_counters_mock_matrix_client.drain_calls();
        assert!(driver_counters_matrix_calls.is_empty());

        let mut driver_gauges_matrix_calls = driver_gauges_mock_matrix_client.drain_calls();
        assert_eq!(
            &driver_gauges_matrix_calls.drain::<i64>("foo_gauge.mean")[..],
            &[TimeMatrixCall::Fold(Timed::now(50))]
        );
        assert_eq!(
            &driver_gauges_matrix_calls.drain::<i64>("foo_gauge.last")[..],
            &[TimeMatrixCall::Fold(Timed::now(50))]
        );
        assert_eq!(
            &driver_gauges_matrix_calls.drain::<i64>("bar_gauge.min")[..],
            &[TimeMatrixCall::Fold(Timed::now(100))]
        );
        assert_eq!(
            &driver_gauges_matrix_calls.drain::<i64>("bar_gauge.sum")[..],
            &[TimeMatrixCall::Fold(Timed::now(100))]
        );
        assert_eq!(
            &driver_gauges_matrix_calls.drain::<i64>("baz_gauge.max")[..],
            &[TimeMatrixCall::Fold(Timed::now(150))]
        );
    }

    #[fuchsia::test]
    fn test_handle_periodic_telemetry_cobalt() {
        let mut test_helper = setup_test();
        let driver_counters_mock_matrix_client = MockTimeMatrixClient::new();
        let driver_gauges_mock_matrix_client = MockTimeMatrixClient::new();
        let logger = ClientIfaceCountersLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            test_helper.monitor_svc_proxy.clone(),
            &test_helper.mock_time_matrix_client,
            driver_counters_mock_matrix_client.clone(),
            driver_gauges_mock_matrix_client.clone(),
        );

        // Transition to IfaceCreated state
        handle_iface_created(&mut test_helper, &logger);

        let mut test_fut = pin!(logger.handle_periodic_telemetry());
        let iface_stats = fidl_stats::IfaceStats {
            connection_stats: Some(fidl_stats::ConnectionStats {
                connection_id: Some(1),
                rx_unicast_total: Some(100),
                rx_unicast_drop: Some(5),
                rx_multicast: Some(30),
                tx_total: Some(50),
                tx_drop: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_iface_stats_req(&mut test_fut, Ok(&iface_stats)),
            Poll::Ready(())
        );

        let metrics = test_helper.get_logged_metrics(metrics::BAD_RX_RATE_METRIC_ID);
        assert!(metrics.is_empty());
        let metrics = test_helper.get_logged_metrics(metrics::RX_UNICAST_PACKETS_METRIC_ID);
        assert!(metrics.is_empty());
        let metrics = test_helper.get_logged_metrics(metrics::BAD_TX_RATE_METRIC_ID);
        assert!(metrics.is_empty());

        let mut test_fut = pin!(logger.handle_periodic_telemetry());
        let iface_stats = fidl_stats::IfaceStats {
            connection_stats: Some(fidl_stats::ConnectionStats {
                connection_id: Some(1),
                rx_unicast_total: Some(200),
                rx_unicast_drop: Some(15),
                rx_multicast: Some(30),
                tx_total: Some(150),
                tx_drop: Some(3),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_iface_stats_req(&mut test_fut, Ok(&iface_stats)),
            Poll::Pending
        );
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );

        let metrics = test_helper.get_logged_metrics(metrics::BAD_RX_RATE_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(1000)); // 10%
        let metrics = test_helper.get_logged_metrics(metrics::RX_UNICAST_PACKETS_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(100));
        let metrics = test_helper.get_logged_metrics(metrics::BAD_TX_RATE_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(100)); // 1%
    }

    #[fuchsia::test]
    fn test_handle_periodic_telemetry_cobalt_changed_connection_id() {
        let mut test_helper = setup_test();
        let driver_counters_mock_matrix_client = MockTimeMatrixClient::new();
        let driver_gauges_mock_matrix_client = MockTimeMatrixClient::new();
        let logger = ClientIfaceCountersLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            test_helper.monitor_svc_proxy.clone(),
            &test_helper.mock_time_matrix_client,
            driver_counters_mock_matrix_client.clone(),
            driver_gauges_mock_matrix_client.clone(),
        );

        // Transition to IfaceCreated state
        handle_iface_created(&mut test_helper, &logger);

        let mut test_fut = pin!(logger.handle_periodic_telemetry());
        let iface_stats = fidl_stats::IfaceStats {
            connection_stats: Some(fidl_stats::ConnectionStats {
                connection_id: Some(1),
                rx_unicast_total: Some(100),
                rx_unicast_drop: Some(5),
                rx_multicast: Some(30),
                tx_total: Some(50),
                tx_drop: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_iface_stats_req(&mut test_fut, Ok(&iface_stats)),
            Poll::Ready(())
        );

        let metrics = test_helper.get_logged_metrics(metrics::BAD_RX_RATE_METRIC_ID);
        assert!(metrics.is_empty());
        let metrics = test_helper.get_logged_metrics(metrics::RX_UNICAST_PACKETS_METRIC_ID);
        assert!(metrics.is_empty());
        let metrics = test_helper.get_logged_metrics(metrics::BAD_TX_RATE_METRIC_ID);
        assert!(metrics.is_empty());

        let mut test_fut = pin!(logger.handle_periodic_telemetry());
        let iface_stats = fidl_stats::IfaceStats {
            connection_stats: Some(fidl_stats::ConnectionStats {
                connection_id: Some(2),
                rx_unicast_total: Some(200),
                rx_unicast_drop: Some(15),
                rx_multicast: Some(30),
                tx_total: Some(150),
                tx_drop: Some(3),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_iface_stats_req(&mut test_fut, Ok(&iface_stats)),
            Poll::Ready(())
        );

        // No metric is logged because the ID indicates it's a different connection, meaning
        // there is nothing to diff with
        let metrics = test_helper.get_logged_metrics(metrics::BAD_RX_RATE_METRIC_ID);
        assert!(metrics.is_empty());
        let metrics = test_helper.get_logged_metrics(metrics::RX_UNICAST_PACKETS_METRIC_ID);
        assert!(metrics.is_empty());
        let metrics = test_helper.get_logged_metrics(metrics::BAD_TX_RATE_METRIC_ID);
        assert!(metrics.is_empty());

        let mut test_fut = pin!(logger.handle_periodic_telemetry());
        let iface_stats = fidl_stats::IfaceStats {
            connection_stats: Some(fidl_stats::ConnectionStats {
                connection_id: Some(2),
                rx_unicast_total: Some(300),
                rx_unicast_drop: Some(18),
                rx_multicast: Some(30),
                tx_total: Some(250),
                tx_drop: Some(5),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_iface_stats_req(&mut test_fut, Ok(&iface_stats)),
            Poll::Pending
        );
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );

        let metrics = test_helper.get_logged_metrics(metrics::BAD_RX_RATE_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(300)); // 3%
        let metrics = test_helper.get_logged_metrics(metrics::RX_UNICAST_PACKETS_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(100));
        let metrics = test_helper.get_logged_metrics(metrics::BAD_TX_RATE_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(200)); // 2%
    }

    #[fuchsia::test]
    fn test_handle_periodic_telemetry_cobalt_suspension_in_between() {
        let mut test_helper = setup_test();
        let driver_counters_mock_matrix_client = MockTimeMatrixClient::new();
        let driver_gauges_mock_matrix_client = MockTimeMatrixClient::new();
        let logger = ClientIfaceCountersLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            test_helper.monitor_svc_proxy.clone(),
            &test_helper.mock_time_matrix_client,
            driver_counters_mock_matrix_client.clone(),
            driver_gauges_mock_matrix_client.clone(),
        );

        // Transition to IfaceCreated state
        handle_iface_created(&mut test_helper, &logger);

        let mut test_fut = pin!(logger.handle_periodic_telemetry());
        let iface_stats = fidl_stats::IfaceStats {
            connection_stats: Some(fidl_stats::ConnectionStats {
                connection_id: Some(1),
                rx_unicast_total: Some(100),
                rx_unicast_drop: Some(5),
                rx_multicast: Some(30),
                tx_total: Some(50),
                tx_drop: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_iface_stats_req(&mut test_fut, Ok(&iface_stats)),
            Poll::Ready(())
        );

        let metrics = test_helper.get_logged_metrics(metrics::BAD_RX_RATE_METRIC_ID);
        assert!(metrics.is_empty());
        let metrics = test_helper.get_logged_metrics(metrics::RX_UNICAST_PACKETS_METRIC_ID);
        assert!(metrics.is_empty());
        let metrics = test_helper.get_logged_metrics(metrics::BAD_TX_RATE_METRIC_ID);
        assert!(metrics.is_empty());

        test_helper.exec.set_fake_boot_to_mono_offset(zx::BootDuration::from_millis(1));
        let mut test_fut = pin!(logger.handle_periodic_telemetry());
        let iface_stats = fidl_stats::IfaceStats {
            connection_stats: Some(fidl_stats::ConnectionStats {
                connection_id: Some(1),
                rx_unicast_total: Some(200),
                rx_unicast_drop: Some(15),
                rx_multicast: Some(30),
                tx_total: Some(150),
                tx_drop: Some(3),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_iface_stats_req(&mut test_fut, Ok(&iface_stats)),
            Poll::Ready(())
        );

        // No metric is logged because the increase in boot-to-mono offset indicates that
        // a suspension had happened in between
        let metrics = test_helper.get_logged_metrics(metrics::BAD_RX_RATE_METRIC_ID);
        assert!(metrics.is_empty());
        let metrics = test_helper.get_logged_metrics(metrics::RX_UNICAST_PACKETS_METRIC_ID);
        assert!(metrics.is_empty());
        let metrics = test_helper.get_logged_metrics(metrics::BAD_TX_RATE_METRIC_ID);
        assert!(metrics.is_empty());

        let mut test_fut = pin!(logger.handle_periodic_telemetry());
        let iface_stats = fidl_stats::IfaceStats {
            connection_stats: Some(fidl_stats::ConnectionStats {
                connection_id: Some(1),
                rx_unicast_total: Some(300),
                rx_unicast_drop: Some(18),
                rx_multicast: Some(30),
                tx_total: Some(250),
                tx_drop: Some(5),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_iface_stats_req(&mut test_fut, Ok(&iface_stats)),
            Poll::Pending
        );
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );

        let metrics = test_helper.get_logged_metrics(metrics::BAD_RX_RATE_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(300)); // 3%
        let metrics = test_helper.get_logged_metrics(metrics::RX_UNICAST_PACKETS_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(100));
        let metrics = test_helper.get_logged_metrics(metrics::BAD_TX_RATE_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(200)); // 2%
    }

    #[fuchsia::test]
    fn test_diff_and_log_rx_cobalt() {
        let mut test_helper = setup_test();
        let prev_stats = fidl_stats::ConnectionStats {
            rx_unicast_total: Some(100),
            rx_unicast_drop: Some(5),
            ..Default::default()
        };
        let current_stats = fidl_stats::ConnectionStats {
            rx_unicast_total: Some(300),
            rx_unicast_drop: Some(7),
            ..Default::default()
        };
        let cobalt_proxy = test_helper.cobalt_1dot1_proxy.clone();
        let mut test_fut = pin!(diff_and_log_rx_cobalt(&cobalt_proxy, &prev_stats, &current_stats));
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );
        let metrics = test_helper.get_logged_metrics(metrics::BAD_RX_RATE_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(100)); // 1%
        let metrics = test_helper.get_logged_metrics(metrics::RX_UNICAST_PACKETS_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(200));
    }

    #[test_case(
        fidl_stats::ConnectionStats { ..Default::default() },
        fidl_stats::ConnectionStats { ..Default::default() };
        "both empty"
    )]
    #[test_case(
        fidl_stats::ConnectionStats {
            rx_unicast_total: Some(100),
            rx_unicast_drop: Some(5),
            ..Default::default()
        },
        fidl_stats::ConnectionStats { ..Default::default() };
        "current empty"
    )]
    #[test_case(
        fidl_stats::ConnectionStats { ..Default::default() },
        fidl_stats::ConnectionStats {
            rx_unicast_total: Some(300),
            rx_unicast_drop: Some(7),
            ..Default::default()
        };
        "prev empty"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_diff_and_log_rx_cobalt_empty(
        prev_stats: fidl_stats::ConnectionStats,
        current_stats: fidl_stats::ConnectionStats,
    ) {
        let mut test_helper = setup_test();
        let cobalt_proxy = test_helper.cobalt_1dot1_proxy.clone();
        let mut test_fut = pin!(diff_and_log_rx_cobalt(&cobalt_proxy, &prev_stats, &current_stats));
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );
        let metrics = test_helper.get_logged_metrics(metrics::BAD_RX_RATE_METRIC_ID);
        assert!(metrics.is_empty())
    }

    #[fuchsia::test]
    fn test_diff_and_log_tx_cobalt() {
        let mut test_helper = setup_test();
        let prev_stats = fidl_stats::ConnectionStats {
            tx_total: Some(100),
            tx_drop: Some(5),
            ..Default::default()
        };
        let current_stats = fidl_stats::ConnectionStats {
            tx_total: Some(300),
            tx_drop: Some(7),
            ..Default::default()
        };
        let cobalt_proxy = test_helper.cobalt_1dot1_proxy.clone();
        let mut test_fut = pin!(diff_and_log_tx_cobalt(&cobalt_proxy, &prev_stats, &current_stats));
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );
        let metrics = test_helper.get_logged_metrics(metrics::BAD_TX_RATE_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(100)); // 1%
    }

    #[test_case(
        fidl_stats::ConnectionStats { ..Default::default() },
        fidl_stats::ConnectionStats { ..Default::default() };
        "both empty"
    )]
    #[test_case(
        fidl_stats::ConnectionStats {
            tx_total: Some(100),
            tx_drop: Some(5),
            ..Default::default()
        },
        fidl_stats::ConnectionStats { ..Default::default() };
        "current empty"
    )]
    #[test_case(
        fidl_stats::ConnectionStats { ..Default::default() },
        fidl_stats::ConnectionStats {
            tx_total: Some(300),
            tx_drop: Some(7),
            ..Default::default()
        };
        "prev empty"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_diff_and_log_tx_cobalt_empty(
        prev_stats: fidl_stats::ConnectionStats,
        current_stats: fidl_stats::ConnectionStats,
    ) {
        let mut test_helper = setup_test();
        let cobalt_proxy = test_helper.cobalt_1dot1_proxy.clone();
        let mut test_fut = pin!(diff_and_log_tx_cobalt(&cobalt_proxy, &prev_stats, &current_stats));
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );
        let metrics = test_helper.get_logged_metrics(metrics::BAD_TX_RATE_METRIC_ID);
        assert!(metrics.is_empty())
    }

    #[fuchsia::test]
    fn test_handle_iface_destroyed() {
        let mut test_helper = setup_test();
        let driver_counters_mock_matrix_client = MockTimeMatrixClient::new();
        let driver_gauges_mock_matrix_client = MockTimeMatrixClient::new();
        let logger = ClientIfaceCountersLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            test_helper.monitor_svc_proxy.clone(),
            &test_helper.mock_time_matrix_client,
            driver_counters_mock_matrix_client.clone(),
            driver_gauges_mock_matrix_client.clone(),
        );

        // Transition to IfaceCreated state
        handle_iface_created(&mut test_helper, &logger);

        let mut handle_iface_destroyed_fut = pin!(logger.handle_iface_destroyed(IFACE_ID));
        assert_eq!(
            test_helper.exec.run_until_stalled(&mut handle_iface_destroyed_fut),
            Poll::Ready(())
        );

        let mut test_fut = pin!(logger.handle_periodic_telemetry());
        assert_eq!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Ready(()));
        let telemetry_svc_stream = test_helper.telemetry_svc_stream.as_mut().unwrap();
        let mut telemetry_svc_req_fut = pin!(telemetry_svc_stream.try_next());
        // Verify that no telemetry request is made now that the iface is destroyed
        match test_helper.exec.run_until_stalled(&mut telemetry_svc_req_fut) {
            Poll::Ready(Ok(None)) => (),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    fn handle_iface_created<S: InspectSender>(
        test_helper: &mut TestHelper,
        logger: &ClientIfaceCountersLogger<S>,
    ) {
        let mut handle_iface_created_fut = pin!(logger.handle_iface_created(IFACE_ID));
        assert_eq!(
            test_helper.run_and_handle_get_sme_telemetry(&mut handle_iface_created_fut),
            Poll::Pending
        );
        let telemetry_support = fidl_stats::TelemetrySupport::default();
        assert_eq!(
            test_helper.run_and_respond_query_telemetry_support(
                &mut handle_iface_created_fut,
                Ok(&telemetry_support)
            ),
            Poll::Ready(())
        );
    }
}
