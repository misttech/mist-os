// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{CurrentTask, ExitStatus};
use fidl_fuchsia_feedback::{
    Annotation, CrashReport, CrashReporterProxy, NativeCrashReport, SpecificCrashReport,
    MAX_ANNOTATION_VALUE_LENGTH,
};
use fuchsia_inspect::Node;
use fuchsia_inspect_contrib::profile_duration;
use fuchsia_zircon::{self as zx, AsHandleRef};
use starnix_logging::{
    log_error, log_info, log_warn, trace_instant, CoreDumpInfo, CoreDumpList, TraceScope,
    CATEGORY_STARNIX,
};
use starnix_sync::Mutex;
use std::sync::Arc;

/// The maximum number of reports we'll allow to be in-flight to the feedback stack at a time.
const MAX_REPORTS_IN_FLIGHT: u8 = 10;

pub struct CrashReporter {
    /// Diagnostics information about crashed tasks.
    core_dumps: CoreDumpList,

    /// Connection to the feedback stack for reporting crashes.
    proxy: Option<CrashReporterProxy>,

    /// Number of reports currently being filed with the feedback stack.
    reports_in_flight: Arc<Mutex<u8>>,
}

impl CrashReporter {
    pub fn new(inspect_node: &Node, proxy: Option<CrashReporterProxy>) -> Self {
        Self {
            core_dumps: CoreDumpList::new(inspect_node.create_child("coredumps")),
            proxy,
            reports_in_flight: Default::default(),
        }
    }

    pub fn handle_core_dump(&self, current_task: &CurrentTask, exit_status: &ExitStatus) {
        profile_duration!("RecordCoreDump");
        trace_instant!(CATEGORY_STARNIX, c"RecordCoreDump", TraceScope::Process);

        let process_koid = current_task
            .thread_group
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
        let linux_pid = current_task.thread_group.leader as i64;
        let argv = current_task
            .read_argv(MAX_ANNOTATION_VALUE_LENGTH as usize)
            .unwrap_or_else(|_| vec!["<unknown>".into()])
            .into_iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>();
        let thread_name = current_task.command().to_string_lossy().into_owned();
        let signal = match exit_status {
            ExitStatus::CoreDump(s) => s.signal,
            other => unreachable!(
                "only core dump exit statuses should be handled as core dumps, got {other:?}"
            ),
        };

        // TODO(https://fxbug.dev/356912301) use boot time
        let uptime = zx::Time::get_monotonic() - current_task.thread_group.start_time;

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

        let argv0 = argv.get(0).map(AsRef::as_ref).unwrap_or_else(|| "<unknown>");
        // Get the filename.
        let argv0 = argv0.rsplit_once("/").unwrap_or(("", &argv0)).1.to_string();

        let mut argv_joined = argv.join(" ");
        truncate_annotation(&mut argv_joined);

        let mut env_joined = current_task
            .read_env(MAX_ANNOTATION_VALUE_LENGTH as usize)
            .unwrap_or_else(|_| vec![])
            .into_iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        truncate_annotation(&mut env_joined);

        let crash_report = CrashReport {
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
                Annotation { key: "linux.signal".to_string(), value: signal.to_string() },
            ]),
            is_fatal: Some(true),
            ..Default::default()
        };

        if let Some(reporter) = &self.proxy {
            let reporter = reporter.clone();
            if let Some(guard) = self.report_guard() {
                // Do the actual report in the background since they can take a while to file.
                current_task.kernel().kthreads.spawn_future(async move {
                    match reporter.file_report(crash_report).await {
                        Ok(Ok(_)) => (),
                        Ok(Err(filing_error)) => {
                            log_error!(?filing_error, "Couldn't file crash report.");
                        }
                        Err(fidl_error) => log_warn!(
                            ?fidl_error,
                            "Couldn't file crash report due to error on underlying channel."
                        ),
                    };

                    // Move the guard into the task so that it stays alive while filing.
                    drop(guard);
                });
            } else {
                log_info!(
                    ?crash_report,
                    "Skipping sending crash report, too many already in-flight."
                );
            }
        } else {
            log_info!(?crash_report, "no crash reporter available for crash");
        }
    }

    /// Return a guard for an in-flight report if few enough are in-flight.
    fn report_guard(&self) -> Option<ReportGuard> {
        let mut num_in_flight = self.reports_in_flight.lock();
        if *num_in_flight < MAX_REPORTS_IN_FLIGHT {
            *num_in_flight += 1;
            Some(ReportGuard(self.reports_in_flight.clone()))
        } else {
            None
        }
    }
}

struct ReportGuard(Arc<Mutex<u8>>);

impl Drop for ReportGuard {
    fn drop(&mut self) {
        *self.0.lock() -= 1;
    }
}

fn truncate_annotation(s: &mut String) {
    let original_len = s.len();
    let max_len = MAX_ANNOTATION_VALUE_LENGTH as usize;
    if original_len <= max_len {
        return;
    }
    s.truncate(max_len - 3);
    if s.len() < original_len {
        s.push_str("...");
    }
}
