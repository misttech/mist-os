// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {guest_cli::platform::PlatformServices, guest_cli_args::*};

mod services;
mod vsh;

#[fuchsia::main(logging_tags = ["guest"])]
async fn main() -> Result<(), anyhow::Error> {
    let options: GuestOptions = argh::from_env();
    let services = guest_cli::platform::FuchsiaPlatformServices::new();

    match options.nested {
        SubCommands::Attach(attach_args) => {
            let output = guest_cli::attach::handle_attach(&services, &attach_args).await?;
            println!("{}", output);
            Ok(())
        }
        SubCommands::Launch(launch_args) => {
            let output = guest_cli::launch::handle_launch(&services, &launch_args).await;
            println!("{}", output);
            Ok(())
        }
        SubCommands::Stop(stop_args) => {
            let output = guest_cli::stop::handle_stop(&services, &stop_args).await?;
            println!("{}", output);
            Ok(())
        }
        SubCommands::Balloon(balloon_args) => {
            let output = guest_cli::balloon::handle_balloon(&services, &balloon_args).await;
            println!("{}", output);
            Ok(())
        }
        SubCommands::List(list_args) => {
            let output = guest_cli::list::handle_list(&services, &list_args).await?;
            println!("{}", output);
            Ok(())
        }
        SubCommands::Wipe(wipe_args) => {
            let output = guest_cli::wipe::handle_wipe(&services, &wipe_args).await?;
            println!("{}", output);
            Ok(())
        }
        SubCommands::VsockPerf(vsockperf_args) => {
            let output = guest_cli::vsockperf::handle_vsockperf(&services, &vsockperf_args).await?;
            println!("{}", output);
            Ok(())
        }
        SubCommands::Socat(socat_args) => {
            let output = guest_cli::socat::handle_socat(&services, &socat_args).await;
            println!("{}", output);
            Ok(())
        }
        SubCommands::Vsh(vsh_args) => {
            // SAFETY: These unsafe helpers should only be called once as they take ownership of the
            // Stdin and Stdout file descriptors (and also assume that these FDs exceed the lifetime
            // of the returned object). An additional consequence is that those FDs will be closed
            // on drop, so we call this as early as possible.
            let mut stdin = unsafe { services::get_evented_stdio(services::Stdio::Stdin) };
            let mut stdout = unsafe { services::get_evented_stdio(services::Stdio::Stdout) };
            let mut stderr = unsafe { services::get_evented_stdio(services::Stdio::Stderr) };

            let termina_manager = services.connect_to_manager(GuestType::Termina).await?;

            vsh::handle_vsh(
                &mut stdin,
                &mut stdout,
                &mut stderr,
                termina_manager,
                vsh_args.port,
                vsh_args.container,
                vsh_args.args,
            )
            .await
            .map(|exit_code| std::process::exit(exit_code))
        }
        SubCommands::Mem(mem_args) => {
            let output = guest_cli::mem::handle_mem(&services, mem_args).await?;
            println!("{}", output);
            Ok(())
        }
    }
}
