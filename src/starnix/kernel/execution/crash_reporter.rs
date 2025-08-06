// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{CurrentTask, ExitStatus};
use fidl_fuchsia_feedback::{
    Annotation, CrashReport, CrashReporterProxy, NativeCrashReport, SpecificCrashReport,
    MAX_ANNOTATION_VALUE_LENGTH, MAX_CRASH_SIGNATURE_LENGTH,
};
use fuchsia_inspect::{Inspector, Node};
use fuchsia_inspect_contrib::profile_duration;
use futures::FutureExt;
use starnix_logging::{
    log_error, log_info, log_warn, trace_instant, CoreDumpInfo, CoreDumpList, TraceScope,
    CATEGORY_STARNIX,
};
use starnix_sync::Mutex;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use zx::{self as zx, AsHandleRef};

/// The maximum number of reports we'll allow to be in-flight to the feedback stack at a time.
const MAX_REPORTS_IN_FLIGHT: u8 = 10;

/// The maximum number of crashes we allow to happen for a process within the last
/// CrashReporter.crash_loop_age_out before we consider it to be crash looping. 8 within 8 minutes
/// was chosen as a balance between "definitely a crash loop" and "still saves system resources."
const CRASH_LOOP_LIMIT: usize = 8;

/// While throttled, we should still occasionally file a report with a higher "weight" that can
/// represent the rest of the crashes.
const REPORT_EVERY_X_WHILE_THROTTLED: u32 = 10;

pub struct CrashReporter {
    /// Diagnostics information about crashed tasks.
    core_dumps: CoreDumpList,

    /// Diagnostics information. A mapping from process name -> number of crashes for that process
    /// that weren't uploaded because of process throttling.
    throttled_core_dumps: Arc<Mutex<HashMap<String, i64>>>,

    /// Tracks when crashes occurred for each process name.
    crashes_per_process: Arc<Mutex<HashMap<String, CrashInfo>>>,

    /// Connection to the feedback stack for reporting crashes.
    proxy: Option<CrashReporterProxy>,

    /// Number of reports currently being filed with the feedback stack.
    /// TODO(https://fxbug.dev/417249552): remove.
    reports_in_flight: Arc<Mutex<u8>>,

    /// The period before a crash is no longer considered for detecting crash loops.
    crash_loop_age_out: zx::MonotonicDuration,

    /// Whether excessive crash reports should be throttled.
    enable_throttling: bool,
}

pub struct PendingCrashReport {
    /// The current task's argv.
    argv: Vec<String>,

    /// The crashed process name.
    argv0: String,

    guard: Option<ReportGuard>,

    /// How many crashes this report represents. For example, a value of 10 would indicate that
    /// this report will represent 9 other throttled crashes for this process.
    weight: u32,
}

impl CrashReporter {
    pub fn new(
        inspect_node: &Node,
        proxy: Option<CrashReporterProxy>,
        crash_loop_age_out: zx::MonotonicDuration,
        enable_throttling: bool,
    ) -> Self {
        let crash_reporter = Self {
            core_dumps: CoreDumpList::new(inspect_node.create_child("coredumps")),
            throttled_core_dumps: Arc::new(Mutex::new(Default::default())),
            crashes_per_process: Arc::new(Mutex::new(Default::default())),
            proxy,
            reports_in_flight: Default::default(),
            crash_loop_age_out,
            enable_throttling,
        };

        crash_reporter.record_throttling_in_inspect(inspect_node);
        crash_reporter
    }

    /// Returns a PendingCrashReport if the crash report should be reported. Otherwise, returns
    /// None.
    pub fn begin_crash_report(&self, current_task: &CurrentTask) -> Option<PendingCrashReport> {
        let argv = current_task
            .read_argv(MAX_ANNOTATION_VALUE_LENGTH as usize)
            .unwrap_or_else(|_| vec!["<unknown>".into()])
            .into_iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>();
        let argv0 = argv.get(0).map(AsRef::as_ref).unwrap_or_else(|| "<unknown>");

        // Get the filename.
        let argv0 = argv0.rsplit_once("/").unwrap_or(("", &argv0)).1.to_string();

        self.report_guard(argv, argv0, zx::MonotonicInstant::get())
    }

    /// Callers should first check whether the crash should be reported via begin_crash_report.
    pub fn handle_core_dump(
        &self,
        current_task: &CurrentTask,
        exit_status: &ExitStatus,
        pending_crash_report: PendingCrashReport,
    ) {
        profile_duration!("RecordCoreDump");
        trace_instant!(CATEGORY_STARNIX, c"RecordCoreDump", TraceScope::Process);

        let argv = pending_crash_report.argv;
        let argv0 = pending_crash_report.argv0;
        let process_koid = current_task
            .thread_group()
            .process
            .get_koid()
            .expect("handles for processes with crashing threads are still valid");
        let thread_koid = current_task
            .thread
            .read()
            .as_ref()
            .expect("coredumps occur in tasks with associated threads")
            .get_koid()
            .expect("handles for crashing threads are still valid");
        let linux_pid = current_task.thread_group().leader as i64;
        let thread_name = current_task.command().to_string_lossy().into_owned();
        let signal = match exit_status {
            ExitStatus::CoreDump(s) => s.signal,
            other => unreachable!(
                "only core dump exit statuses should be handled as core dumps, got {other:?}"
            ),
        };

        // TODO(https://fxbug.dev/356912301) use boot time
        let uptime = zx::MonotonicInstant::get() - current_task.thread_group().start_time;

        let dump_info = CoreDumpInfo {
            process_koid,
            thread_koid,
            linux_pid,
            uptime: uptime.into_nanos(),
            argv: argv.clone(),
            thread_name: thread_name.clone(),
            signal: signal.to_string(),
        };
        self.core_dumps.record_core_dump(dump_info);

        let mut argv_joined = argv.join(" ");
        truncate_with_ellipsis(&mut argv_joined, MAX_ANNOTATION_VALUE_LENGTH as usize);

        let mut env_joined = current_task
            .read_env(MAX_ANNOTATION_VALUE_LENGTH as usize)
            .unwrap_or_else(|_| vec![])
            .into_iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        truncate_with_ellipsis(&mut env_joined, MAX_ANNOTATION_VALUE_LENGTH as usize);

        let signal_str = signal.to_string();

        // Truncate program name to fit in crash signature with a space and signal string added.
        let max_signature_prefix_len = MAX_CRASH_SIGNATURE_LENGTH as usize - (signal_str.len() + 1);
        let mut crash_signature = argv0.clone();
        truncate_with_ellipsis(&mut crash_signature, max_signature_prefix_len);
        crash_signature.push(' ');
        crash_signature.push_str(&signal_str);

        let crash_report = CrashReport {
            crash_signature: Some(crash_signature),
            program_name: Some(argv0.clone()),
            program_uptime: Some(uptime.into_nanos()),
            specific_report: Some(SpecificCrashReport::Native(NativeCrashReport {
                process_koid: Some(process_koid.raw_koid()),
                process_name: Some(argv0),
                thread_koid: Some(thread_koid.raw_koid()),
                thread_name: Some(thread_name),
                ..Default::default()
            })),
            annotations: Some(vec![
                // Note that this pid will be different from the Zircon process koid that's visible
                // to the rest of Fuchsia. We want to include both so that this can be correlated
                // against debugging artifacts produced by Android code.
                Annotation { key: "linux.pid".to_string(), value: linux_pid.to_string() },
                Annotation { key: "linux.argv".to_string(), value: argv_joined },
                Annotation { key: "linux.env".to_string(), value: env_joined },
                Annotation { key: "linux.signal".to_string(), value: signal_str },
            ]),
            is_fatal: Some(true),
            weight: Some(pending_crash_report.weight),
            ..Default::default()
        };

        if let Some(reporter) = &self.proxy {
            let reporter = reporter.clone();
            // Do the actual report in the background since they can take a while to file.
            current_task.kernel().kthreads.spawn_future(async move {
                match reporter.file_report(crash_report).await {
                    Ok(Ok(_)) => (),
                    Ok(Err(filing_error)) => {
                        log_error!(filing_error:?; "Couldn't file crash report.");
                    }
                    Err(fidl_error) => log_warn!(
                        fidl_error:?;
                        "Couldn't file crash report due to error on underlying channel."
                    ),
                };

                if let Some(guard) = pending_crash_report.guard {
                    // Move the guard into the task so that it stays alive while filing.
                    drop(guard);
                };
            });
        } else {
            log_info!(crash_report:?; "no crash reporter available for crash");
        }
    }

    /// Locally records that a crash for `process_name` occurred at `runtime` and returns a guard
    /// for an in-flight report if few enough overall are in-flight, as well as the weight that
    /// should be assigned to the crash report.
    ///
    /// Note: runtime is the total time the device has been on according to the monotonic clock, not
    /// the amount of time the process was running.
    fn report_guard(
        &self,
        argv: Vec<String>,
        argv0: String,
        runtime: zx::MonotonicInstant,
    ) -> Option<PendingCrashReport> {
        if !self.enable_throttling {
            return Some(PendingCrashReport { argv, argv0, guard: None, weight: 1 });
        }

        // Locally record that the crash occurred.
        let mut crashes_per_process = self.crashes_per_process.lock();
        let crash_info = crashes_per_process.entry(argv0.clone()).or_default();
        crash_info.crash_runtimes.push_back(runtime);

        crash_info.prune_crash_runtimes(runtime, self.crash_loop_age_out);

        // Even if we're not throttled, we still need to have a weight of 1 so incrementing this
        // here will let us later use it as the weight.
        crash_info.num_crashes_while_throttled += 1;

        // Check if this particular process has been filing too many reports.
        if crash_info.is_throttled_at(runtime, self.crash_loop_age_out)
            && (crash_info.num_crashes_while_throttled < REPORT_EVERY_X_WHILE_THROTTLED)
        {
            log_info!("Process '{argv0}' is throttled due to suspected crash loop, will fold report into later crash");
            *self.throttled_core_dumps.lock().entry(argv0).or_default() += 1;
            return None;
        }

        // Check if Starnix as a whole has been filing too many reports.
        let mut num_in_flight = self.reports_in_flight.lock();
        if *num_in_flight < MAX_REPORTS_IN_FLIGHT {
            *num_in_flight += 1;

            let weight = crash_info.num_crashes_while_throttled;
            crash_info.num_crashes_while_throttled = 0;

            Some(PendingCrashReport {
                argv,
                argv0,
                guard: Some(ReportGuard(self.reports_in_flight.clone())),
                weight,
            })
        } else {
            log_info!(
                "Skipping sending crash report for process '{argv0}', too many already in-flight for Starnix."
            );
            None
        }
    }

    fn record_throttling_in_inspect(&self, inspect_node: &Node) {
        let throttled_core_dumps = self.throttled_core_dumps.clone();
        let crashes_per_process = self.crashes_per_process.clone();
        let crash_loop_age_out = self.crash_loop_age_out;

        inspect_node.record_lazy_child("coredumps_throttled", move || {
            let throttled_core_dumps = throttled_core_dumps.clone();
            let crashes_per_process = crashes_per_process.clone();

            async move {
                let inspector = Inspector::default();
                let mut crashes_per_process = crashes_per_process.lock();
                let runtime = zx::MonotonicInstant::get();

                for (process, count) in throttled_core_dumps.lock().iter() {
                    let Some(crash_info) = crashes_per_process.get_mut(process) else {
                        continue;
                    };

                    crash_info.prune_crash_runtimes(runtime, crash_loop_age_out);

                    let process_node = inspector.root().create_child(process);
                    process_node.record_bool(
                        "currently_throttled",
                        crash_info.is_throttled_at(runtime, crash_loop_age_out),
                    );
                    process_node.record_int("total_throttled_crashes", *count);
                    if let Some(end) = crash_info.throttling_end(crash_loop_age_out) {
                        process_node.record_int("throttling_runtime_end_millis", end.into_millis());
                    }

                    inspector.root().record(process_node);
                }
                Ok(inspector)
            }
            .boxed()
        });
    }
}

#[derive(Default)]
struct CrashInfo {
    /// How many crashes have occurred while throttled. Resets to 0 if the throttling ends or if a
    /// representative report is uploaded every REPORT_EVERY_X_WHILE_THROTTLED.
    num_crashes_while_throttled: u32,

    /// When the crashes occurred. Crashes that occurred more than CrashReporter.crash_loop_age_out
    /// ago may be removed.
    crash_runtimes: VecDeque<zx::MonotonicInstant>,
}

impl CrashInfo {
    /// Whether the process is throttled at a given instant.
    fn is_throttled_at(
        &self,
        runtime: zx::MonotonicInstant,
        crash_loop_age_out: zx::MonotonicDuration,
    ) -> bool {
        self.crash_runtimes.iter().filter(|&&x| (runtime - x) < crash_loop_age_out).count()
            > CRASH_LOOP_LIMIT
    }

    /// When a process will no longer be throttled, if it currently is throttled.
    fn throttling_end(
        &self,
        crash_loop_age_out: zx::MonotonicDuration,
    ) -> Option<zx::MonotonicDuration> {
        let throttling_end = self.crash_runtimes.iter().nth_back(CRASH_LOOP_LIMIT - 1)?;
        Some(crash_loop_age_out + zx::Duration::from_nanos(throttling_end.into_nanos()))
    }

    // Only keeps entries that are within `crash_loop_age_out`.
    fn prune_crash_runtimes(
        &mut self,
        runtime: zx::MonotonicInstant,
        crash_loop_age_out: zx::MonotonicDuration,
    ) {
        self.crash_runtimes.retain(|&x| (runtime - x) < crash_loop_age_out);
    }
}

struct ReportGuard(Arc<Mutex<u8>>);

impl Drop for ReportGuard {
    fn drop(&mut self) {
        *self.0.lock() -= 1;
    }
}

fn truncate_with_ellipsis(s: &mut String, max_len: usize) {
    if s.len() <= max_len {
        return;
    }

    // 3 bytes for ellipsis.
    let max_content_len = max_len - 3;

    // String::truncate panics if the new max length is in the middle of a character, so we need to
    // find an appropriate byte boundary.
    let mut new_len = 0;
    let mut iter = s.char_indices();
    while let Some((offset, _)) = iter.next() {
        if offset > max_content_len {
            break;
        }
        new_len = offset;
    }

    s.truncate(new_len);
    s.push_str("...");
}

#[cfg(test)]
mod tests {
    use crate::testing::spawn_kernel_and_run;

    use super::*;

    const CRASH_LOOP_AGE_OUT: zx::MonotonicDuration = zx::Duration::from_minutes(8);

    #[test]
    fn truncate_noop_on_max_length_string() {
        let mut s = String::from("1234567890");
        let before = s.clone();
        truncate_with_ellipsis(&mut s, 10);
        assert_eq!(s, before);
    }

    #[test]
    fn truncate_adds_ellipsis() {
        let mut s = String::from("1234567890");
        truncate_with_ellipsis(&mut s, 9);
        assert_eq!(s.len(), 9);
        assert_eq!(s, "123456...", "truncate must add ellipsis and still fit under max len");
    }

    #[test]
    fn truncate_is_sensible_in_middle_of_multibyte_chars() {
        let mut s = String::from("æææææææææ");
        // æ is 2 bytes, so any odd byte length should be in the middle of a character. Truncate
        // adds 3 bytes for the ellipsis so we actually need an even max length to hit the middle
        // of a character.
        truncate_with_ellipsis(&mut s, 8);
        assert_eq!(s.len(), 7, "may end up shorter than provided max length w/ multi-byte chars");
        assert_eq!(s, "ææ...", "truncate must remove whole characters and add ellipsis");
    }

    #[test]
    fn not_throttled() {
        let crash_reporter = CrashReporter::new(
            &fuchsia_inspect::Node::default(),
            /*proxy=*/ None,
            CRASH_LOOP_AGE_OUT,
            /*enable_throttling=*/ true,
        );

        assert!(crash_reporter
            .report_guard(vec![], "test-process".to_string(), zx::Instant::from_nanos(0))
            .is_some());
    }

    #[test]
    fn throttled() {
        let crash_reporter = CrashReporter::new(
            &fuchsia_inspect::Node::default(),
            /*proxy=*/ None,
            CRASH_LOOP_AGE_OUT,
            /*enable_throttling=*/ true,
        );

        for _ in 0..CRASH_LOOP_LIMIT {
            assert!(crash_reporter
                .report_guard(vec![], "test-process".to_string(), zx::Instant::from_nanos(0))
                .is_some());
        }
        assert!(crash_reporter
            .report_guard(vec![], "test-process".to_string(), zx::Instant::from_nanos(0))
            .is_none());
    }

    #[test]
    fn throttling_ages_out() {
        let crash_reporter = CrashReporter::new(
            &fuchsia_inspect::Node::default(),
            /*proxy=*/ None,
            CRASH_LOOP_AGE_OUT,
            /*enable_throttling=*/ true,
        );

        for _ in 0..CRASH_LOOP_LIMIT {
            assert!(crash_reporter
                .report_guard(vec![], "test-process".to_string(), zx::Instant::from_nanos(0))
                .is_some());
        }
        assert!(crash_reporter
            .report_guard(vec![], "test-process".to_string(), zx::Instant::from_nanos(0))
            .is_none());
        assert!(crash_reporter
            .report_guard(
                vec![],
                "test-process".to_string(),
                zx::Instant::from_nanos(CRASH_LOOP_AGE_OUT.into_nanos())
            )
            .is_some());
    }

    #[fuchsia::test]
    async fn begin_crash_report_throttling_ends() {
        spawn_kernel_and_run(|_, current_task| {
            let crash_reporter = CrashReporter::new(
                &fuchsia_inspect::Node::default(),
                /*proxy=*/ None,
                zx::Duration::from_millis(200),
                /*enable_throttling=*/ true,
            );

            for _ in 0..CRASH_LOOP_LIMIT {
                assert!(crash_reporter.begin_crash_report(current_task).is_some());
            }
            assert!(crash_reporter.begin_crash_report(current_task).is_none());

            std::thread::sleep(std::time::Duration::from_millis(250));
            assert!(crash_reporter.begin_crash_report(current_task).is_some());
        });
    }

    #[test]
    fn reports_some_crashes_while_throttled() {
        const RUNTIME: zx::MonotonicInstant = zx::Instant::from_nanos(0);
        let crash_reporter = CrashReporter::new(
            &fuchsia_inspect::Node::default(),
            /*proxy=*/ None,
            CRASH_LOOP_AGE_OUT,
            /*enable_throttling=*/ true,
        );

        for _ in 0..CRASH_LOOP_LIMIT {
            assert!(crash_reporter
                .report_guard(vec![], "test-process".to_string(), RUNTIME)
                .is_some());
        }

        for _ in 0..REPORT_EVERY_X_WHILE_THROTTLED - 1 {
            assert!(crash_reporter
                .report_guard(vec![], "test-process".to_string(), RUNTIME)
                .is_none());
        }

        assert_eq!(
            crash_reporter
                .report_guard(vec![], "test-process".to_string(), RUNTIME)
                .unwrap()
                .weight,
            REPORT_EVERY_X_WHILE_THROTTLED
        );
    }

    #[test]
    fn is_throttled_filters() {
        let mut crash_info: CrashInfo = Default::default();

        crash_info.crash_runtimes.push_back(zx::MonotonicInstant::from_nanos(0));
        for _ in 0..CRASH_LOOP_LIMIT {
            crash_info.crash_runtimes.push_back(zx::MonotonicInstant::from_nanos(50));
        }

        assert!(crash_info.is_throttled_at(zx::MonotonicInstant::from_nanos(0), CRASH_LOOP_AGE_OUT));
        assert!(!crash_info.is_throttled_at(
            zx::MonotonicInstant::from_nanos(CRASH_LOOP_AGE_OUT.into_nanos()),
            CRASH_LOOP_AGE_OUT
        ));
    }
}
