// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
use anyhow::{anyhow, bail, Result};
use fidl_fuchsia_developer_ffx::TargetInfo;
use std::io::{stdin, stdout, BufRead, Write};

pub(crate) async fn choose_target(
    targets: Vec<TargetInfo>,
    def: Option<String>,
) -> Result<TargetInfo> {
    // If they specified a target...
    if let Some(t) = def {
        let filtered_targets = targets
            .iter()
            .filter(|x| x.nodename.as_ref().unwrap() == &t)
            .cloned()
            .collect::<Vec<TargetInfo>>();
        let found_target = filtered_targets.first();
        if found_target.is_none() {
            anyhow::bail!("Specified target does not exist")
        }
        return Ok(found_target.cloned().unwrap());
    }

    match targets.len() {
        0 => {
            bail!("No targets discovered")
        }
        1 => {
            let first = targets.first();
            match first {
                Some(f) => Ok(f.clone()),
                None => bail!("No targets"),
            }
        }
        _ => {
            // Okay there is more than one target available and they haven't
            // specified a target to use, lets prompt them for one
            let stdin = stdin().lock();
            prompt_for_target(stdin, stdout(), targets).await
        }
    }
}

async fn prompt_for_target<R, W>(
    mut input: R,
    mut output: W,
    targets: Vec<TargetInfo>,
) -> Result<TargetInfo>
where
    R: BufRead,
    W: Write,
{
    writeln!(output, "Multiple devices detected. Please choose the target by number:")?;
    for (i, t) in targets.iter().enumerate() {
        let name = t.clone().nodename.unwrap_or("unknown".to_string());
        writeln!(output, "  {i}: {name}")?;
    }
    write!(output, "Choice: ")?;
    output.flush()?;

    let mut choice = String::new();
    input.read_line(&mut choice)?;
    let idx: usize = choice
        .trim()
        .parse()
        .map_err(|_| anyhow!("Choice: {} was not an unsigned integer", choice))?;
    if idx == 0 || idx > targets.len() {
        bail!("Invalid choice: {}", idx);
    }

    targets
        .get(idx - 1)
        .ok_or_else(|| anyhow!("Could not get target {} from collection", (idx - 1)))
        .cloned()
}

#[cfg(test)]
mod test {
    use super::*;
    use pretty_assertions::assert_eq;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_prompt_for_target() -> Result<()> {
        let input = b"1";
        let output = Vec::new();
        let targets = vec![
            TargetInfo { nodename: Some("cytherera".to_string()), ..Default::default() },
            TargetInfo { nodename: Some("alecto".to_string()), ..Default::default() },
        ];

        let res = prompt_for_target(&input[..], output, targets).await?;
        assert_eq!(
            res,
            TargetInfo { nodename: Some("cytherera".to_string()), ..Default::default() }
        );
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_prompt_for_target_not_usize_error() -> Result<()> {
        let input = b"asdf";
        let output = Vec::new();
        let targets = vec![
            TargetInfo { nodename: Some("cytherera".to_string()), ..Default::default() },
            TargetInfo { nodename: Some("alecto".to_string()), ..Default::default() },
        ];

        let res = prompt_for_target(&input[..], output, targets).await;
        assert!(res.is_err());
        assert_eq!(format!("{}", res.unwrap_err()), "Choice: asdf was not an unsigned integer");
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_prompt_for_target_out_of_range_error() -> Result<()> {
        let input = b"4";
        let output = Vec::new();
        let targets = vec![
            TargetInfo { nodename: Some("cytherera".to_string()), ..Default::default() },
            TargetInfo { nodename: Some("alecto".to_string()), ..Default::default() },
        ];

        let res = prompt_for_target(&input[..], output, targets).await;
        assert!(res.is_err());
        assert_eq!(format!("{}", res.unwrap_err()), "Invalid choice: 4");
        Ok(())
    }
}
