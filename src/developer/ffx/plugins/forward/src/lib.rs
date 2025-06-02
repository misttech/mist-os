// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::VecDeque;
use std::fmt::Display;
use std::time::Duration;

use errors::ffx_error;
use ffx_forward_args::{Direction, ForwardCommand, ForwardSpec, ProtoSpec};
use ffx_target_net::{Bidirectional, Counters, PortForwarder};
use ffx_writer::SimpleWriter;
use fho::{FfxMain, FfxTool};
use fuchsia_async as fasync;
use futures::FutureExt as _;
use log::{error, info};
use speedtest::{BytesFormatter, Throughput};
use target_holders::RemoteControlProxyHolder;

use termion as _;

#[derive(FfxTool)]
pub struct ForwardTool {
    remote_control: RemoteControlProxyHolder,
    #[command]
    cmd: ForwardCommand,
}

fho::embedded_plugin!(ForwardTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ForwardTool {
    type Writer = SimpleWriter;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let Self { remote_control, cmd } = self;
        let forwarder = PortForwarder::new_with_rcs(Duration::from_secs(10), &*remote_control)
            .await
            .map_err(|e| ffx_error!(e))?;
        let ForwardCommand { quiet, spec, ui_interval } = cmd;
        if spec.is_empty() {
            return Err(fho::user_error!("no forwarding specs provided"));
        }
        for ForwardSpec { host, target, direction } in spec {
            info!(
                "setting up forwarding host: {:?}, target: {:?}, direction: {:?}",
                host, target, direction
            );
            match direction {
                Direction::HostToTarget => {
                    let listener = match host {
                        ProtoSpec::Tcp(addr) => tokio::net::TcpListener::bind(addr)
                            .await
                            .map_err(|e| fho::user_error!(e))?,
                    };
                    let target_addr = match target {
                        ProtoSpec::Tcp(addr) => addr,
                    };
                    fasync::Task::local(
                        forwarder
                            .forward(listener, target_addr)
                            .map(|r| r.unwrap_or_else(|e| error!("forwarding error: {e:?}"))),
                    )
                    .detach();
                }
                Direction::TargetToHost => {
                    let listener = match target {
                        ProtoSpec::Tcp(addr) => forwarder
                            .socket_provider()
                            .listen(addr, None)
                            .await
                            .map_err(|e| fho::user_error!(e))?,
                    };
                    let host_addr = match host {
                        ProtoSpec::Tcp(addr) => addr,
                    };
                    fasync::Task::local(
                        forwarder.reverse(listener, host_addr).map(|r| {
                            r.unwrap_or_else(|e| error!("reverse forwarding error: {e:?}"))
                        }),
                    )
                    .detach();
                }
            }
        }

        if quiet {
            return Ok(futures::future::pending().await);
        }

        let mut renderer = Renderer::default();

        loop {
            fasync::Timer::new(ui_interval).await;
            renderer
                .render(forwarder.read_counters(), ui_interval, &mut writer)
                .map_err(|e| ffx_error!(e))?;
        }
    }
}

#[derive(Default)]
struct Renderer {
    prev_bytes: Option<Bidirectional>,
    period: usize,
    max_throughput: f64,
    host_to_target: BarRenderer,
    target_to_host: BarRenderer,
}

impl Renderer {
    const PERIOD_SPINNER: [char; 2] = ['⇋', '⇌'];
    const RENDER_LINES: u16 = 3;
    const MIN_BARS: u16 = 5;

    fn render<W: std::io::Write>(
        &mut self,
        new_counters: Counters,
        delta: Duration,
        w: &mut W,
    ) -> std::io::Result<()> {
        let bar_width = termion::terminal_size()
            .map(|(w, _)| w.saturating_sub(35).max(Self::MIN_BARS))
            .unwrap_or(Self::MIN_BARS)
            .into();
        let Self {
            prev_bytes,
            period,
            max_throughput,
            host_to_target: host_to_target_render,
            target_to_host: target_to_host_render,
        } = self;
        let Counters { active_connections, total_bytes } = new_counters;
        let interval_bytes = match prev_bytes {
            Some(Bidirectional { host_to_target, target_to_host }) => {
                write!(w, "{}", termion::cursor::Up(Self::RENDER_LINES))?;
                Bidirectional {
                    host_to_target: total_bytes.host_to_target.saturating_sub(*host_to_target),
                    target_to_host: total_bytes.target_to_host.saturating_sub(*target_to_host),
                }
            }
            None => Bidirectional::default(),
        };
        let spinner = Self::PERIOD_SPINNER[*period];
        let host_to_target_throughput = Throughput::from_len_and_duration(
            interval_bytes.host_to_target.try_into().unwrap_or(u32::MAX),
            delta,
        );
        let target_to_host_throughput = Throughput::from_len_and_duration(
            interval_bytes.target_to_host.try_into().unwrap_or(u32::MAX),
            delta,
        );
        *max_throughput = max_throughput
            .max(host_to_target_throughput.as_f64())
            .max(target_to_host_throughput.as_f64());
        host_to_target_render.update(
            host_to_target_throughput.as_f64(),
            bar_width,
            *max_throughput,
        );
        target_to_host_render.update(
            target_to_host_throughput.as_f64(),
            bar_width,
            *max_throughput,
        );

        writeln!(
            w,
            "{}({}) → Host to Target {} ({}) ← Target to Host",
            termion::clear::CurrentLine,
            active_connections.host_to_target,
            spinner,
            active_connections.target_to_host,
        )?;
        writeln!(
            w,
            "{}→ |{}| {} / {}",
            termion::clear::CurrentLine,
            host_to_target_render,
            host_to_target_throughput,
            BytesFormatter(total_bytes.host_to_target.try_into().unwrap_or(u64::MAX)),
        )?;
        writeln!(
            w,
            "{}← |{}| {} / {}",
            termion::clear::CurrentLine,
            target_to_host_render,
            target_to_host_throughput,
            BytesFormatter(total_bytes.target_to_host.try_into().unwrap_or(u64::MAX)),
        )?;

        *prev_bytes = Some(total_bytes);
        *period = (*period + 1) % Self::PERIOD_SPINNER.len();
        Ok(())
    }
}

#[derive(Default)]
struct BarRenderer {
    history: VecDeque<f64>,
    scale: f64,
}

impl BarRenderer {
    const THROUGHPUT_BARS: [char; 7] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇'];

    fn update(&mut self, v: f64, width: usize, scale: f64) {
        self.scale = scale;
        while self.history.len() < width - 1 {
            self.history.push_front(0f64);
        }
        self.history.push_back(v);
        while self.history.len() > width {
            self.history.pop_front();
        }
    }
}

impl Display for BarRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for v in &self.history {
            let ch = if *v == 0f64 {
                ' '
            } else {
                let bar_index = (v / self.scale * (Self::THROUGHPUT_BARS.len() as f64)) as usize;
                Self::THROUGHPUT_BARS[bar_index.min(Self::THROUGHPUT_BARS.len() - 1)]
            };
            write!(f, "{ch}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use vte::{Parser, Perform};

    const INTERVAL: Duration = Duration::from_secs(1);

    #[derive(Default)]
    struct TestWriter {
        data: InnerWriter,
        parser: Parser,
    }

    impl std::io::Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.parser.advance(&mut self.data, buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl TestWriter {
        fn to_string(&mut self) -> String {
            String::from_utf8(self.data.lines.join("\n".as_bytes())).expect("invalid string")
        }
    }

    #[derive(Default)]
    struct InnerWriter {
        lines: Vec<Vec<u8>>,
        cur_line: usize,
    }

    impl InnerWriter {
        fn get_line(&mut self) -> &mut Vec<u8> {
            if self.lines.len() <= self.cur_line {
                self.lines.resize_with(self.cur_line + 1, Vec::new);
            }
            &mut self.lines[self.cur_line]
        }
    }

    impl Perform for InnerWriter {
        fn print(&mut self, c: char) {
            let mut buf = [0u8; 4];
            let len = c.len_utf8();
            c.encode_utf8(&mut buf);
            self.get_line().extend_from_slice(&buf[..len]);
        }

        fn execute(&mut self, c: u8) {
            const NEWLINE: u8 = '\n' as u8;
            match c {
                NEWLINE => {
                    self.cur_line += 1;
                    // Ensure the line is created in the buffer.
                    let _ = self.get_line();
                }
                c => panic!("unrecognized control char: {c}"),
            }
        }

        fn csi_dispatch(
            &mut self,
            params: &vte::Params,
            _intermediates: &[u8],
            ignore: bool,
            action: char,
        ) {
            assert!(!ignore);
            match action {
                // Cursor up.
                'A' => {
                    let param = params.iter().next().expect("missing param");
                    let param = assert_matches!(param, [p] => *p);
                    self.cur_line = self.cur_line.checked_sub(param.into()).expect("bad cursor up");
                }
                // Clear line.
                'K' => {
                    self.get_line().clear();
                }
                c => panic!("unrecognized csi sequence: {c}"),
            }
        }

        fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
            unimplemented!("escape sequences not implemented: {intermediates:?}, {byte}")
        }
    }

    #[test]
    fn renderer_idle() {
        let mut renderer = Renderer::default();
        let mut writer = TestWriter::default();
        let counters = Counters::default();
        renderer.render(counters, INTERVAL, &mut writer).unwrap();
        pretty_assertions::assert_eq!(
            writer.to_string(),
            "(0) → Host to Target ⇋ (0) ← Target to Host\n\
            → |     | 0.0 bps / 0.0 B\n\
            ← |     | 0.0 bps / 0.0 B\n"
        );
        renderer.render(counters, INTERVAL, &mut writer).unwrap();
    }

    #[test]
    fn renderer_traffic() {
        let mut renderer = Renderer::default();
        let mut writer = TestWriter::default();

        let mut start_counters = Counters {
            active_connections: Bidirectional { host_to_target: 5, target_to_host: 2 },
            total_bytes: Default::default(),
        };
        let mut counters = |host_to_target, target_to_host| {
            start_counters.total_bytes.host_to_target += host_to_target;
            start_counters.total_bytes.target_to_host += target_to_host;
            start_counters.clone()
        };

        // Don't have a delta to calculate in the first round, so there's no
        // throughput info.
        renderer.render(counters(100, 200), INTERVAL, &mut writer).unwrap();
        pretty_assertions::assert_eq!(
            writer.to_string(),
            "(5) → Host to Target ⇋ (2) ← Target to Host\n\
            → |     | 0.0 bps / 100.0 B\n\
            ← |     | 0.0 bps / 200.0 B\n"
        );
        renderer.render(counters(100, 200), INTERVAL, &mut writer).unwrap();
        pretty_assertions::assert_eq!(
            writer.to_string(),
            "(5) → Host to Target ⇌ (2) ← Target to Host\n\
            → |    ▄| 800.0 bps / 200.0 B\n\
            ← |    ▇| 1.6 Kbps / 400.0 B\n"
        );
        renderer.render(counters(0, 0), INTERVAL, &mut writer).unwrap();
        pretty_assertions::assert_eq!(
            writer.to_string(),
            "(5) → Host to Target ⇋ (2) ← Target to Host\n\
            → |   ▄ | 0.0 bps / 200.0 B\n\
            ← |   ▇ | 0.0 bps / 400.0 B\n"
        );
        renderer.render(counters(400, 200), INTERVAL, &mut writer).unwrap();
        pretty_assertions::assert_eq!(
            writer.to_string(),
            "(5) → Host to Target ⇌ (2) ← Target to Host\n\
            → |  ▂ ▇| 3.2 Kbps / 600.0 B\n\
            ← |  ▄ ▄| 1.6 Kbps / 600.0 B\n"
        );
        renderer.render(counters(40, 20), INTERVAL, &mut writer).unwrap();
        pretty_assertions::assert_eq!(
            writer.to_string(),
            "(5) → Host to Target ⇋ (2) ← Target to Host\n\
            → | ▂ ▇▁| 320.0 bps / 640.0 B\n\
            ← | ▄ ▄▁| 160.0 bps / 620.0 B\n"
        );
        renderer.render(counters(0, 0), INTERVAL, &mut writer).unwrap();
        pretty_assertions::assert_eq!(
            writer.to_string(),
            "(5) → Host to Target ⇌ (2) ← Target to Host\n\
            → |▂ ▇▁ | 0.0 bps / 640.0 B\n\
            ← |▄ ▄▁ | 0.0 bps / 620.0 B\n"
        );
        renderer.render(counters(0, 0), INTERVAL, &mut writer).unwrap();
        pretty_assertions::assert_eq!(
            writer.to_string(),
            "(5) → Host to Target ⇋ (2) ← Target to Host\n\
            → | ▇▁  | 0.0 bps / 640.0 B\n\
            ← | ▄▁  | 0.0 bps / 620.0 B\n"
        );
    }

    #[test]
    fn bar_renderer() {
        let mut bar = BarRenderer::default();
        bar.update(1.0, 1, 1.0);
        assert_eq!(bar.to_string(), "▇");
        bar.update(0.5, 5, 1.0);
        assert_eq!(bar.to_string(), "   ▇▄");
        bar.update(0.0, 5, 0.5);
        assert_eq!(bar.to_string(), "  ▇▇ ");
        bar.update(1.0, 2, 1.0);
        assert_eq!(bar.to_string(), " ▇");
        bar.update(0.00001, 2, 1.0);
        assert_eq!(bar.to_string(), "▇▁");
        bar.update(20.0, 2, 1.0);
        assert_eq!(bar.to_string(), "▁▇");
    }
}
