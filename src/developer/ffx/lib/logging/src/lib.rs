// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

const TIME_FORMAT: &str = "%b %d %H:%M:%S%.3f";

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq)]
pub struct LogTimer;

impl tracing_subscriber::fmt::time::FormatTime for LogTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        let time = chrono::Local::now().format(TIME_FORMAT);
        write!(w, "{}", time)
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
        LogFormat { id, display_spans, display_target: true, ..Default::default() }
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
        use tracing_subscriber::fmt::{time::FormatTime, FormatFields};

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
