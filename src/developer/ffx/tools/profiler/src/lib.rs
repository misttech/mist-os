// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
mod args;

use anyhow::{Context as _, Result};
use args::{ProfilerCommand, ProfilerSubCommand};
use async_fs::File;
use errors::{ffx_bail, ffx_error};
use fho::{deferred, moniker, FfxMain, FfxTool, MachineWriter, ToolIO};
use fidl_fuchsia_cpu_profiler as profiler;
use std::{default::Default, io::stdin, io::BufRead, time::Duration};

type Writer = MachineWriter<()>;
#[derive(FfxTool)]
pub struct ProfilerTool {
    #[with(deferred(moniker("/core/profiler")))]
    controller: fho::Deferred<profiler::SessionProxy>,
    #[command]
    cmd: ProfilerCommand,
}

#[async_trait::async_trait(?Send)]
impl FfxMain for ProfilerTool {
    type Writer = Writer;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        Ok(profiler(self.controller, writer, self.cmd).await?)
    }
}

fn gather_targets(opts: &args::Start) -> Result<fidl_fuchsia_cpu_profiler::TargetConfig> {
    if let Some(url) = &opts.url {
        if !opts.pids.is_empty() || !opts.tids.is_empty() || !opts.job_ids.is_empty() {
            ffx_bail!(
                "Targeting both a component and specific jobs/processes/threads is not supported"
            )
        }
        let component_config = profiler::ComponentConfig {
            url: Some(url.clone()),
            moniker: opts.moniker.clone(),
            ..Default::default()
        };
        Ok(profiler::TargetConfig::Component(component_config))
    } else if let Some(moniker) = &opts.moniker {
        if !opts.pids.is_empty() || !opts.tids.is_empty() || !opts.job_ids.is_empty() {
            ffx_bail!(
                "Targeting both a component and specific jobs/processes/threads is not supported"
            )
        }
        let component_config =
            profiler::ComponentConfig { moniker: Some(moniker.clone()), ..Default::default() };
        Ok(profiler::TargetConfig::Component(component_config))
    } else {
        let tasks: Vec<_> = opts
            .job_ids
            .iter()
            .map(|&id| profiler::Task::Job(id))
            .chain(opts.pids.iter().map(|&id| profiler::Task::Process(id)))
            .chain(opts.tids.iter().map(|&id| profiler::Task::Thread(id)))
            .collect();
        if tasks.is_empty() {
            ffx_bail!("No targets were specified")
        }
        Ok(profiler::TargetConfig::Tasks(tasks))
    }
}

pub async fn profiler(
    controller: fho::Deferred<profiler::SessionProxy>,
    mut writer: Writer,
    cmd: ProfilerCommand,
) -> Result<()> {
    match cmd.sub_cmd {
        ProfilerSubCommand::Start(opts) => {
            let target = gather_targets(&opts)?;
            let profiler_config = profiler::Config {
                configs: Some(vec![]),
                target: Some(target),
                ..Default::default()
            };

            let (client, server) = fidl::Socket::create_stream();
            let client = fidl::AsyncSocket::from_socket(client).context("making async socket")?;
            let controller = controller.await?;
            controller
                .configure(profiler::SessionConfigureRequest {
                    output: Some(server),
                    config: Some(profiler_config),
                    ..Default::default()
                })
                .await?
                .map_err(|e| ffx_error!("Failed to start: {:?}", e))?;

            let copy_task = fuchsia_async::Task::local(async {
                let mut out_file = File::create(opts.output).await.unwrap();
                futures::io::copy(client, &mut out_file).await
            });

            controller
                .start(&profiler::SessionStartRequest {
                    buffer_results: Some(true),
                    ..Default::default()
                })
                .await?
                .map_err(|e| ffx_error!("Failed to start: {:?}", e))?;

            if let &Some(duration) = &opts.duration {
                writer.line(format!("Waiting for {} seconds...", duration))?;
                fuchsia_async::Timer::new(Duration::from_secs_f64(duration)).await;
            } else {
                writer.line("Press <enter> to stop profiling...")?;
                blocking::unblock(|| {
                    let _ = stdin().lock().read_line(&mut String::new());
                })
                .await;
            }
            let stats = controller.stop().await?;
            if opts.print_stats {
                writer.line(format!("\nSession Stats: "))?;
                if let Some(num_samples) = stats.samples_collected {
                    writer.line(format!("    Num of samples collected: {}", num_samples))?;
                }
                if let Some(median_sample_time) = stats.median_sample_time {
                    writer.line(format!("    Median sample time: {}us", median_sample_time))?;
                }
                if let Some(mean_sample_time) = stats.mean_sample_time {
                    writer.line(format!("    Mean sample time: {}us", mean_sample_time))?;
                }
                if let Some(max_sample_time) = stats.max_sample_time {
                    writer.line(format!("    Max sample time: {}us", max_sample_time))?;
                }
                if let Some(min_sample_time) = stats.min_sample_time {
                    writer.line(format!("    Min sample time: {}us", min_sample_time))?;
                }
            }
            copy_task.await?;
            controller.reset().await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_gather_targets() {
        let args = args::Start {
            pids: vec![1, 2, 3],
            tids: vec![4, 5, 6],
            job_ids: vec![7, 8, 9],
            url: None,
            moniker: None,
            duration: None,
            output: String::from("output_file"),
            print_stats: false,
        };
        let target = gather_targets(&args);
        match target {
            Ok(fidl_fuchsia_cpu_profiler::TargetConfig::Tasks(vec)) => assert!(vec.len() == 9),
            _ => assert!(false),
        }

        let empty_args = args::Start {
            pids: vec![],
            tids: vec![],
            job_ids: vec![],
            moniker: None,
            url: None,
            duration: None,
            output: String::from("output_file"),
            print_stats: false,
        };

        let empty_targets = gather_targets(&empty_args);
        assert!(empty_targets.is_err());

        let invalid_args1 = args::Start {
            pids: vec![1],
            tids: vec![],
            job_ids: vec![],
            moniker: Some(String::from("core/test")),
            url: None,
            duration: None,
            output: String::from("output_file"),
            print_stats: false,
        };
        let invalid_args2 = args::Start {
            pids: vec![],
            tids: vec![1],
            job_ids: vec![],
            moniker: Some(String::from("core/test")),
            url: None,
            duration: None,
            output: String::from("output_file"),
            print_stats: false,
        };
        let invalid_args3 = args::Start {
            pids: vec![],
            tids: vec![],
            job_ids: vec![1],
            moniker: Some(String::from("core/test")),
            url: None,
            duration: None,
            output: String::from("output_file"),
            print_stats: false,
        };

        let invalid_targets1 = gather_targets(&invalid_args1);
        assert!(invalid_targets1.is_err());
        let invalid_targets2 = gather_targets(&invalid_args2);
        assert!(invalid_targets2.is_err());
        let invalid_targets3 = gather_targets(&invalid_args3);
        assert!(invalid_targets3.is_err());
    }
}
