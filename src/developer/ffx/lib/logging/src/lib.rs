// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

const TIME_FORMAT: &str = "%b %d %H:%M:%S%.3f";

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq)]
pub struct LogTimer;

impl tracing_subscriber::fmt::time::FormatTime for LogTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        let time = self.time_formatted();
        write!(w, "{}", time)
    }
}

impl LogTimer {
    fn time_formatted(&self) -> String {
        chrono::Local::now().format(TIME_FORMAT).to_string()
    }
}

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq)]
pub struct LogFormat {
    id: u64,
    display_thread_id: bool,
    display_filename: bool,
    display_line_number: bool,
    display_spans: bool,
    display_target: bool,
    timer: LogTimer,
}

impl LogFormat {
    pub fn new(id: u64, display_spans: bool) -> Self {
        LogFormat { id, display_spans, ..Default::default() }
    }
}

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for LogFormat
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        use tracing_log::NormalizeEvent;
        use tracing_subscriber::fmt::time::FormatTime;
        use tracing_subscriber::fmt::FormatFields;

        let normalized_meta = event.normalized_metadata();
        let meta = normalized_meta.as_ref().unwrap_or_else(|| event.metadata());

        if self.timer.format_time(&mut writer).is_err() {
            writer.write_str("<unknown time>")?;
        }
        writer.write_char(' ')?;

        write!(writer, "[{:0>20?}] ", self.id)?;

        match *meta.level() {
            tracing::Level::TRACE => write!(writer, "TRACE ")?,
            tracing::Level::DEBUG => write!(writer, "DEBUG ")?,
            tracing::Level::INFO => write!(writer, "INFO ")?,
            tracing::Level::WARN => write!(writer, "WARN ")?,
            tracing::Level::ERROR => write!(writer, "ERROR ")?,
        }

        if self.display_thread_id {
            write!(writer, "{:0>2?} ", std::thread::current().id())?;
        }

        if self.display_spans {
            let full_ctx = FullCtx::new(ctx, event.parent());
            write!(writer, "{}", full_ctx)?;
        }

        if self.display_target {
            write!(writer, "{}: ", meta.target())?;
        }

        let line_number = if self.display_line_number { meta.line() } else { None };

        if self.display_filename {
            if let Some(filename) = meta.file() {
                write!(writer, "{}:{}", filename, if line_number.is_some() { "" } else { " " })?;
            }
        }

        if let Some(line_number) = line_number {
            write!(writer, "{}: ", line_number)?;
        }

        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

struct FullCtx<'a, S, N>
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    N: for<'writer> tracing_subscriber::fmt::FormatFields<'writer> + 'static,
{
    ctx: &'a tracing_subscriber::fmt::FmtContext<'a, S, N>,
    span: Option<&'a tracing::span::Id>,
}

impl<'a, S, N: 'a> FullCtx<'a, S, N>
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    N: for<'writer> tracing_subscriber::fmt::FormatFields<'writer> + 'static,
{
    fn new(
        ctx: &'a tracing_subscriber::fmt::FmtContext<'a, S, N>,
        span: Option<&'a tracing::span::Id>,
    ) -> Self {
        Self { ctx, span }
    }
}

impl<'a, S, N> std::fmt::Display for FullCtx<'a, S, N>
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    N: for<'writer> tracing_subscriber::fmt::FormatFields<'writer> + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::fmt::Write;
        let mut seen = false;

        let span = self.span.and_then(|id| self.ctx.span(id)).or_else(|| self.ctx.lookup_current());

        let scope = span.into_iter().flat_map(|span| span.scope().from_root());

        for span in scope {
            write!(f, "{}", span.metadata().name())?;
            seen = true;

            let ext = span.extensions();
            let fields = &ext
                .get::<tracing_subscriber::fmt::FormattedFields<N>>()
                .expect("Unable to find FormattedFields in extensions; this is a bug");
            if !fields.is_empty() {
                write!(f, "{{{}}}", fields)?;
            }
            f.write_char(':')?;
        }

        if seen {
            f.write_char(' ')?;
        }
        Ok(())
    }
}

pub mod log_based {
    use super::LogTimer;
    use std::io::Write;
    use std::marker::{Send, Sync};
    use std::sync::{Arc, Mutex};

    pub trait Filter {
        fn should_emit(&self, _record: &log::Metadata<'_>) -> bool {
            true
        }
    }

    impl Filter for log::LevelFilter {
        fn should_emit(&self, metadata: &log::Metadata<'_>) -> bool {
            match self.to_level() {
                None => false,
                Some(level) => {
                    if level >= metadata.level() {
                        true
                    } else {
                        false
                    }
                }
            }
        }
    }

    pub struct TargetsFilter {
        targets: Vec<(String, log::LevelFilter)>,
    }

    impl TargetsFilter {
        pub fn new(targets: Vec<(String, log::LevelFilter)>) -> Self {
            Self { targets }
        }
    }

    impl Filter for TargetsFilter {
        fn should_emit(&self, metadata: &log::Metadata<'_>) -> bool {
            if self.targets.len() == 0 {
                true
            } else {
                for (target, level) in &self.targets {
                    if metadata.target().starts_with(target) && *level >= metadata.level() {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Implements a compact formatter which also knows about tags.
    #[allow(dead_code)]
    #[derive(Default)]
    pub struct FormatOpts {
        id: u64,
        display_thread_id: bool,
        display_filename: bool,
        display_line_number: bool,
        display_target: bool,
        tags: Vec<String>,
        timer: LogTimer,
    }

    impl FormatOpts {
        pub fn new(id: u64) -> Self {
            FormatOpts { id, ..Default::default() }
        }
    }

    pub struct FfxLog<F> {
        format: FormatOpts,
        sinks: Vec<Box<dyn LogSinkTrait>>,
        main_filter: F,
        level: log::LevelFilter,
        targets_filter: TargetsFilter,
    }

    impl<F: Filter + Send + Sync + 'static> FfxLog<F> {
        /// Constructs a new `FfxLog` with a minimum severity configured.
        pub fn new(
            sinks: Vec<Box<dyn LogSinkTrait>>,
            format: FormatOpts,
            main_filter: F,
            level: log::LevelFilter,
            targets_filter: TargetsFilter,
        ) -> Self {
            Self { sinks, format, main_filter, level, targets_filter }
        }
    }

    pub trait LogSinkTrait: Send + 'static + Sync {
        fn write_record(&self, opts: &FormatOpts, record: &log::Record<'_>);
        fn flush(&self);
        fn boxed(self) -> Box<dyn LogSinkTrait + Send + Sync + 'static>
        where
            Self: Sized,
            Self: LogSinkTrait + Send + Sync + 'static,
        {
            Box::new(self)
        }
    }

    pub struct FfxLogSink<W: Write> {
        writable: Arc<Mutex<W>>,
    }

    impl<W: Write + Send + 'static> FfxLogSink<W> {
        pub fn new(w: Arc<Mutex<W>>) -> Self {
            Self { writable: w }
        }
    }

    impl<W: Write + Send + 'static> LogSinkTrait for FfxLogSink<W> {
        fn write_record(&self, opts: &FormatOpts, record: &log::Record<'_>) {
            let mut writer = self.writable.lock().unwrap();
            let _ = write!(writer, "{}", opts.timer.time_formatted());

            let meta = record.metadata();

            let _ = write!(writer, "[{:0>20?}] ", opts.id);
            let level = match meta.level() {
                log::Level::Trace => "TRACE ",
                log::Level::Debug => "DEBUG ",
                log::Level::Info => "INFO ",
                log::Level::Warn => "WARN ",
                log::Level::Error => "ERROR ",
            };
            let _ = write!(writer, "{}", level);

            if opts.display_thread_id {
                let _ = write!(writer, "{:0>2?} ", std::thread::current().id());
            }

            if opts.display_target {
                let _ = write!(writer, "{}: ", meta.target());
            }

            let line_number = if opts.display_line_number { record.line() } else { None };

            if opts.display_filename {
                if let Some(filename) = record.file() {
                    let _ = write!(
                        writer,
                        "{}:{}",
                        filename,
                        if line_number.is_some() { "" } else { " " }
                    );
                }
            }

            if let Some(line_number) = line_number {
                let _ = write!(writer, "{}: ", line_number);
            }

            let _ = write!(writer, "{}", record.args());
            let mut visitor = StringVisitor::new(&mut *writer);
            let _ = record.key_values().visit(&mut visitor);

            let _ = writeln!(writer);
        }

        fn flush(&self) {
            let _ = self.writable.lock().unwrap().flush();
        }
    }

    impl<F: Filter + Sync + Send + 'static> log::Log for FfxLog<F> {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            self.main_filter.should_emit(metadata)
                && self.targets_filter.should_emit(metadata)
                && self.level.should_emit(metadata)
        }

        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                for sink in &self.sinks {
                    sink.write_record(&self.format, record);
                }
            }
        }

        fn flush(&self) {
            for sink in &self.sinks {
                sink.flush()
            }
        }
    }

    pub(crate) struct StringVisitor<'a, W>(&'a mut W);

    impl<'a, W> StringVisitor<'a, W> {
        pub(crate) fn new(writer: &'a mut W) -> Self {
            Self(writer)
        }
    }

    impl<W: Write> log::kv::VisitSource<'_> for StringVisitor<'_, W> {
        fn visit_pair(
            &mut self,
            key: log::kv::Key<'_>,
            value: log::kv::Value<'_>,
        ) -> Result<(), log::kv::Error> {
            value.visit(StringValueVisitor { buf: self.0, key: key.as_str() })
        }
    }

    struct StringValueVisitor<'a, W> {
        buf: &'a mut W,
        key: &'a str,
    }

    impl<W: Write> log::kv::VisitValue<'_> for StringValueVisitor<'_, W> {
        fn visit_any(&mut self, value: log::kv::Value<'_>) -> Result<(), log::kv::Error> {
            write!(self.buf, " {}={}", self.key, value)
                .expect("writing into strings does not fail");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::log_based::{
        FfxLog, FfxLogSink, Filter, FormatOpts, LogSinkTrait, StringVisitor, TargetsFilter,
    };
    use log::{Level, Log, Record};
    use std::sync::{Arc, Mutex, RwLock};

    //////////////////////////////////////////////////////////////////////
    /// Visitors
    ///

    #[test]
    fn test_stringvaluevisitor_writes_nothing_if_no_kv() {
        let mut buf = vec![];
        let mut visitor = StringVisitor::new(&mut buf);
        let record = Record::builder()
            .args(format_args!("Error!"))
            .level(Level::Error)
            .target("myApp")
            .file(Some("server.rs"))
            .line(Some(144))
            .module_path(Some("server"))
            .build();
        record.key_values().visit(&mut visitor).unwrap();
        assert_eq!(buf, vec![]);
    }

    #[test]
    fn test_stringvaluevisitor_kvs() {
        let mut buf = vec![];
        let mut visitor = StringVisitor::new(&mut buf);

        let source = &[("a", 1), ("b", 2), ("c", 3)];
        let record = Record::builder()
            .args(format_args!("Error!"))
            .level(Level::Error)
            .target("myApp")
            .file(Some("server.rs"))
            .line(Some(144))
            .key_values(source)
            .module_path(Some("server"))
            .build();
        record.key_values().visit(&mut visitor).unwrap();
        assert_eq!(String::from_utf8(buf).expect("Valid UTF8"), " a=1 b=2 c=3".to_string());
    }

    //////////////////////////////////////////////////////////////////////
    /// LevelFilter
    ///

    #[test]
    fn test_levelfilter_should_emit() {
        let level = log::LevelFilter::Debug;
        let metadata = log::Metadata::builder().level(Level::Warn).build();
        assert!(level.should_emit(&metadata));
    }
    #[test]
    fn test_levelfilter_shouldnot_emit() {
        let level = log::LevelFilter::Error;
        let metadata = log::Metadata::builder().level(Level::Warn).build();
        assert!(!level.should_emit(&metadata));
    }

    //////////////////////////////////////////////////////////////////////
    /// TargetsFilter
    ///

    #[test]
    fn test_targetsfilter_should_emit_on_empty() {
        let targets = TargetsFilter::new(vec![]);
        let metadata = log::Metadata::builder().level(Level::Warn).target("coronabeth").build();
        assert!(targets.should_emit(&metadata));
    }

    #[test]
    fn test_targetsfilter_should_emit_on_prefix() {
        let targets = TargetsFilter::new(vec![("cor".to_string(), log::LevelFilter::Debug)]);
        let metadata = log::Metadata::builder().level(Level::Warn).target("coronabeth").build();
        assert!(targets.should_emit(&metadata));
    }

    #[test]
    fn test_targetsfilter_should_not_emit_on_prefix_mismatch() {
        let targets = TargetsFilter::new(vec![("ianthe".to_string(), log::LevelFilter::Debug)]);
        let metadata = log::Metadata::builder().level(Level::Warn).target("coronabeth").build();
        assert!(!targets.should_emit(&metadata));
    }

    #[test]
    fn test_targetsfilter_should_not_emit_on_level() {
        let targets = TargetsFilter::new(vec![("coro".to_string(), log::LevelFilter::Debug)]);
        let metadata = log::Metadata::builder().level(Level::Trace).target("coronabeth").build();
        assert!(!targets.should_emit(&metadata));
    }

    //////////////////////////////////////////////////////////////////////
    /// FfxLog
    ///

    struct SwitchFilter {
        should: Arc<RwLock<bool>>,
    }

    impl Filter for SwitchFilter {
        fn should_emit(&self, _metadata: &log::Metadata<'_>) -> bool {
            match *self.should.read().unwrap() {
                true => true,
                false => false,
            }
        }
    }

    #[test]
    fn test_ffx_log_emitting() {
        let mut sinks: Vec<Box<dyn LogSinkTrait>> = vec![];

        let buf1 = Arc::new(Mutex::new(Vec::<u8>::new()));
        let buf2 = Arc::new(Mutex::new(Vec::<u8>::new()));
        sinks.push(FfxLogSink::new(buf1.clone()).boxed());
        sinks.push(FfxLogSink::new(buf2.clone()).boxed());

        let should = Arc::new(RwLock::new(true));
        let switch_filter = SwitchFilter { should: should.clone() };
        let opts = FormatOpts::new(1234);

        let level_filter = log::LevelFilter::Trace;
        let targets_filter = TargetsFilter::new(vec![]);

        let log = FfxLog::new(sinks, opts, switch_filter, level_filter, targets_filter);

        let source = &[("a", 1), ("b", 2), ("c", 3)];
        let record = Record::builder()
            .args(format_args!("Error!"))
            .level(Level::Error)
            .target("myApp")
            .file(Some("server.rs"))
            .line(Some(144))
            .key_values(source)
            .module_path(Some("server"))
            .build();
        log.log(&record);

        let buf1_done = buf1.lock().unwrap();
        let buf2_done = buf2.lock().unwrap();

        assert_eq!(
            String::from_utf8((buf1_done).to_vec()).expect("Valid UTF8"),
            String::from_utf8((buf2_done).to_vec()).expect("Valid UTF8"),
        );
    }

    #[test]
    fn test_ffx_log_no_emitting() {
        let mut sinks: Vec<Box<dyn LogSinkTrait>> = vec![];

        let buf1 = Arc::new(Mutex::new(Vec::<u8>::new()));
        sinks.push(FfxLogSink::new(buf1.clone()).boxed());

        let should = Arc::new(RwLock::new(true));
        let binding = Arc::clone(&should);
        let switch_filter = SwitchFilter { should: Arc::clone(&should) };
        let opts = FormatOpts::new(1234);

        let level_filter = log::LevelFilter::Trace;
        let targets_filter = TargetsFilter::new(vec![]);

        let log = FfxLog::new(sinks, opts, switch_filter, level_filter, targets_filter);

        let source = &[("a", 1), ("b", 2), ("c", 3)];
        let record = Record::builder()
            .args(format_args!("Error!"))
            .level(Level::Error)
            .target("myApp")
            .file(Some("server.rs"))
            .line(Some(144))
            .key_values(source)
            .module_path(Some("server"))
            .build();
        assert!(log.enabled(record.metadata()));
        log.log(&record);
        {
            let mut buf = buf1.lock().unwrap();
            assert!(buf.len() > 0);
            buf.clear();
        }
        {
            let mut sg = binding.write().unwrap();
            *sg = false;
        }
        assert!(!log.enabled(record.metadata()));
        log.log(&record);
        {
            let buf = buf1.lock().unwrap();
            assert!(buf.len() == 0);
        }
    }
}
