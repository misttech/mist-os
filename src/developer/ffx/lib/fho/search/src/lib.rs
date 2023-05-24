// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_command::{
    FfxCommandLine, FfxContext, FfxToolInfo, FfxToolSource, MetricsSession, Result, ToolRunner,
    ToolSuite,
};
use ffx_config::{EnvironmentContext, Sdk, SelectMode};
use fho_metadata::FhoToolMetadata;
use serde_json::Value;

use std::{
    collections::HashMap,
    fs::File,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::ExitStatus,
};

/// The config key for holding subtool search paths.
pub const FFX_SUBTOOL_PATHS_CONFIG: &str = "ffx.subtool-search-paths";

/// Path information about a subtool
#[derive(Clone, Debug)]
struct SubToolLocation {
    source: FfxToolSource,
    name: String,
    tool_path: PathBuf,
    metadata_path: PathBuf,
}

/// A subtool discovered in a user's workspace or sdk
#[derive(Clone)]
pub struct ExternalSubTool {
    cmd_line: FfxCommandLine,
    context: EnvironmentContext,
    path: PathBuf,
}

#[derive(Clone)]
pub struct ExternalSubToolSuite {
    context: EnvironmentContext,
    workspace_tools: HashMap<String, SubToolLocation>,
}

#[async_trait::async_trait(?Send)]
impl ToolRunner for ExternalSubTool {
    fn forces_stdout_log(&self) -> bool {
        false
    }

    async fn run(self: Box<Self>, _metrics: MetricsSession) -> Result<ExitStatus> {
        // fho v0: Run the exact same command, just with the first argument
        // replaced with the 'real' tool location. We will also exec() it so
        // we don't have to do signal management, but later versions of fho
        // will likely need to do more here.
        let exec_err = std::process::Command::new(&self.path)
            .env(EnvironmentContext::FFX_BIN_ENV, self.context.rerun_bin().await?)
            .args(self.cmd_line.ffx_args_iter().chain(self.cmd_line.subcmd_iter()))
            .exec();

        // Because we use exec above, we are only ever here if something went
        // wrong with the exec. We will never return Ok() for this function with
        // fho v0.
        Err(exec_err).bug_context("Running external subtool")
        // note: we specifically do not want to report metrics here, as we're running the command externally.
        // The final command is the one that knows how to redact its own args, so it will do it itself.
    }
}

impl ExternalSubToolSuite {
    /// Load subtools from `subtool_paths` and use `context` for the environment context.
    /// This is used both by the main implementation of [`ExternalSubToolSuite::from_env`] and
    /// in tests to redirect to different subtool paths.
    fn with_tools_from(
        context: EnvironmentContext,
        subtool_paths: &[impl AsRef<Path>],
    ) -> Result<Self> {
        let workspace_tools =
            find_workspace_tools(subtool_paths).map(|tool| (tool.name.to_owned(), tool)).collect();
        Ok(Self { context, workspace_tools })
    }

    fn find_workspace_tool(&self, ffx_cmd: &FfxCommandLine) -> Option<ExternalSubTool> {
        let name = ffx_cmd.global.subcommand.first()?;
        let cmd = match self.workspace_tools.get(name).and_then(SubToolLocation::validate_tool) {
            Some(FfxToolInfo { path: Some(path), .. }) => {
                let context = self.context.clone();
                let cmd_line = ffx_cmd.clone();
                let path = path.clone();
                ExternalSubTool { cmd_line, context, path }
            }
            _ => return None,
        };
        Some(cmd)
    }

    fn find_sdk_tool(&self, sdk: &Sdk, ffx_cmd: &FfxCommandLine) -> Option<ExternalSubTool> {
        let name = format!("ffx-{}", ffx_cmd.global.subcommand.first()?);
        let ffx_tool = sdk.get_ffx_tool(&name)?;
        let location = SubToolLocation::from_path(
            FfxToolSource::Sdk,
            &ffx_tool.executable,
            &ffx_tool.metadata,
        )?;
        let Some(FfxToolInfo { path: Some(path), .. }) = location.validate_tool() else { return None };
        let context = self.context.clone();
        let cmd_line = ffx_cmd.clone();
        Some(ExternalSubTool { cmd_line, context, path })
    }
}

#[async_trait::async_trait(?Send)]
impl ToolSuite for ExternalSubToolSuite {
    async fn from_env(env: &EnvironmentContext) -> Result<Self> {
        let subtool_config: Vec<Value> = env
            .query(FFX_SUBTOOL_PATHS_CONFIG)
            .select(SelectMode::All)
            .get_file()
            .await
            .unwrap_or_else(|_| vec![]);
        Self::with_tools_from(env.clone(), &get_subtool_paths(subtool_config))
    }

    fn global_command_list() -> &'static [&'static argh::CommandInfo] {
        &[]
    }

    async fn command_list(&self) -> Vec<FfxToolInfo> {
        let mut tools: Vec<_> = self.workspace_tools.values().cloned().collect();
        if let Ok(sdk) = self.context.get_sdk().await {
            for ffx_tool in sdk.get_ffx_tools() {
                SubToolLocation::from_path(
                    FfxToolSource::Sdk,
                    &ffx_tool.executable,
                    &ffx_tool.metadata,
                )
                .map(|loc| tools.push(loc));
            }
        }
        tools.iter().filter_map(SubToolLocation::validate_tool).collect()
    }

    async fn try_from_args(
        &self,
        ffx_cmd: &FfxCommandLine,
    ) -> Result<Option<Box<(dyn ToolRunner + '_)>>> {
        // look in the workspace first
        if let Some(cmd) = self.find_workspace_tool(ffx_cmd) {
            return Ok(Some(Box::new(cmd)));
        }
        // then try the sdk
        let sdk_cmd =
            self.context.get_sdk().await.ok().and_then(|sdk| self.find_sdk_tool(&sdk, ffx_cmd));
        if let Some(cmd) = sdk_cmd {
            return Ok(Some(Box::new(cmd)));
        }
        // and we're done
        Ok(None)
    }
}

/// Loads a list of subtool paths from an array of values, flattening them into
/// a list of [`PathBuf`]s.
fn get_subtool_paths(subtools: Vec<Value>) -> Vec<PathBuf> {
    use Value::*;
    subtools
        .into_iter()
        .flat_map(|val| match val {
            Array(arr) => arr.into_iter(),
            other => vec![other].into_iter(),
        })
        .filter_map(|val| val.as_str().map(PathBuf::from))
        .collect()
}

/// Searches a set of directories for tools matching the path `ffx-<name>`
/// and returns information about them based on known abis
fn find_workspace_tools<P>(subtool_paths: &[P]) -> impl Iterator<Item = SubToolLocation> + '_
where
    P: AsRef<Path>,
{
    subtool_paths
        .iter()
        .filter_map(|path| {
            Some(std::fs::read_dir(path.as_ref()).ok()?.filter_map(move |entry| {
                let entry = entry.ok()?;
                SubToolLocation::from_path(
                    FfxToolSource::Workspace,
                    &entry.path(),
                    &entry.path().with_extension("json"),
                )
            }))
        })
        .flatten()
}

impl SubToolLocation {
    /// Evaluate the given path for if it looks like a subtool based on filename and the
    /// presence of a metadata file.
    fn from_path(
        source: FfxToolSource,
        tool_path: &Path,
        metadata_path: &Path,
    ) -> Option<SubToolLocation> {
        let file_name = tool_path.file_name()?.to_str()?;
        if let Some(suffix) = file_name.strip_prefix("ffx-") {
            let name = suffix.to_lowercase();
            // require the presence of a metadata file
            if metadata_path.exists() {
                let tool_path = tool_path.to_owned();
                let metadata_path = metadata_path.to_owned();
                return Some(SubToolLocation { source, name, tool_path, metadata_path });
            }
        }
        None
    }

    /// Loads the details of the metadata from the file to validate that it is a runnable
    /// command with the current fho version and obtaining extra metadata from the metadata
    /// file.
    ///
    /// Doing this in two steps avoids reading files unnecessarily until we want to either
    /// run one or list it.
    fn validate_tool(&self) -> Option<FfxToolInfo> {
        // bail early if for whatever reason we can't read the metadata.
        let metadata: FhoToolMetadata =
            File::open(&self.metadata_path).ok().and_then(|f| serde_json::from_reader(f).ok())?;
        // also if it requires an fho version we don't support
        metadata.is_supported()?;
        // ignore the tool if the metadata's name is incorrect
        if metadata.name == self.name {
            let source = self.source;
            let name = metadata.name;
            let description = metadata.description;
            let path = Some(self.tool_path.to_owned());
            Some(FfxToolInfo { source, name, description, path })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_command::Ffx;
    use fho_metadata::{FhoDetails, Only};
    use serde_json::json;
    use std::{collections::HashSet, io::Write};

    enum MockMetadata<'a> {
        Valid(FhoToolMetadata),
        Invalid(&'a str),
        NotThere,
    }
    use MockMetadata::*;

    fn check_ffx_tool(source: FfxToolSource, path: &Path) -> Option<FfxToolInfo> {
        SubToolLocation::from_path(source, path, &path.with_extension("json"))
            .as_ref()
            .and_then(SubToolLocation::validate_tool)
    }

    // Sets up a mock subtool in `dir` with the name `subtool_name` and, adjacent metadata based on the
    // `metadata` argument.
    fn create_mock_subtool(dir: &Path, subtool_name: &str, metadata: MockMetadata<'_>) -> PathBuf {
        let subtool_path = dir.join(subtool_name);
        let metadata_path = subtool_path.with_extension("json");
        File::create(&subtool_path).expect("creating subtool file");
        match metadata {
            Valid(meta) => {
                let file = File::create(&metadata_path).expect("creating subtool metadata");
                serde_json::to_writer(file, &meta).expect("Writing subtool metadata")
            }
            Invalid(s) => {
                let mut file =
                    File::create(&metadata_path).expect("creating invalid subtool metadata");
                write!(file, "{s}").expect("Writing invalid subtool metadata")
            }
            _ => {}
        }
        subtool_path
    }

    #[test]
    fn check_non_existent() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        assert!(
            check_ffx_tool(FfxToolSource::Workspace, &tempdir.path().join("ffx-non-existent"))
                .is_none(),
            "Non-existent subtool should be None"
        );
    }

    #[test]
    fn check_no_metadata() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let name = "ffx-no-metadata";
        let subtool = create_mock_subtool(tempdir.path(), name, NotThere);
        assert!(
            check_ffx_tool(FfxToolSource::Workspace, &subtool).is_none(),
            "Tool with no metadata should be None"
        );
    }

    #[test]
    fn check_invalid_metadata() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let name = "ffx-bad-metadata";
        let subtool = create_mock_subtool(tempdir.path(), name, Invalid("boom"));
        assert!(
            check_ffx_tool(FfxToolSource::Workspace, &subtool).is_none(),
            "Tool with bad metadata should be None"
        );
    }

    #[test]
    fn check_valid_metadata() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let name = "ffx-valid-metadata";
        let metadata = FhoToolMetadata::new("valid-metadata", "A tool with valid metadata!");
        let subtool = create_mock_subtool(tempdir.path(), name, Valid(metadata.clone()));
        let info = FfxToolInfo {
            source: FfxToolSource::Workspace,
            name: metadata.name.clone(),
            description: metadata.description.clone(),
            path: Some(subtool.clone()),
        };
        assert_eq!(
            check_ffx_tool(FfxToolSource::Workspace, &subtool),
            Some(info),
            "Tool with valid metadata should be what we put in"
        );
    }

    #[test]
    fn check_incorrect_name_metadata() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let name = "ffx-invalid-metadata";
        let metadata = FhoToolMetadata::new("not-the-right-name", "A tool with invalid metadata!");
        let subtool = create_mock_subtool(tempdir.path(), name, Valid(metadata.clone()));
        assert_eq!(
            check_ffx_tool(FfxToolSource::Workspace, &subtool),
            None,
            "Tool with invalid metadata should be None"
        );
    }

    #[test]
    fn check_future_fho_version_required() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let name = "ffx-invalid-metadata";
        let metadata = FhoToolMetadata {
            name: "invalid-metadata".to_owned(),
            description: "A tool with invalid metadata!".to_owned(),
            requires_fho: u16::MAX,
            fho_details: FhoDetails::FhoVersion0 { version: Only },
        };
        let subtool = create_mock_subtool(tempdir.path(), name, Valid(metadata.clone()));
        assert_eq!(
            check_ffx_tool(FfxToolSource::Workspace, &subtool),
            None,
            "Tool with invalid metadata should be None"
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn scan_workspace_subtool_directory() {
        let test_env = ffx_config::test_init().await.expect("test init");

        let tempdir = tempfile::tempdir().expect("tempdir");
        create_mock_subtool(
            tempdir.path(),
            "ffx-something",
            Valid(FhoToolMetadata::new("something", "something something something")),
        );
        create_mock_subtool(
            tempdir.path(),
            "ffx-something-else",
            Valid(FhoToolMetadata::new("something-else", "something something something else")),
        );
        create_mock_subtool(
            tempdir.path(),
            "ffx-whatever",
            Valid(FhoToolMetadata::new("whatever", "whatevs")),
        );
        create_mock_subtool(
            tempdir.path(),
            "ffx-orelse",
            Valid(FhoToolMetadata::new("orelse", "what")),
        );

        let suite =
            ExternalSubToolSuite::with_tools_from(test_env.context.clone(), &[tempdir.path()])
                .expect("subtool suite scanning should succeed");

        assert!(
            ExternalSubToolSuite::global_command_list().is_empty(),
            "no global commands for an external suite"
        );

        let basic_subtool_definition = FfxToolInfo {
            source: FfxToolSource::Workspace,
            name: "".to_string(),
            description: "".to_string(),
            path: None,
        };
        let expected_commands: HashSet<_> = HashSet::from_iter(
            [
                FfxToolInfo {
                    name: "something".to_owned(),
                    description: "something something something".to_owned(),
                    path: Some(tempdir.path().join("ffx-something")),
                    ..basic_subtool_definition
                },
                FfxToolInfo {
                    name: "something-else".to_owned(),
                    description: "something something something else".to_owned(),
                    path: Some(tempdir.path().join("ffx-something-else")),
                    ..basic_subtool_definition
                },
                FfxToolInfo {
                    name: "whatever".to_owned(),
                    description: "whatevs".to_owned(),
                    path: Some(tempdir.path().join("ffx-whatever")),
                    ..basic_subtool_definition
                },
                FfxToolInfo {
                    name: "orelse".to_owned(),
                    description: "what".to_owned(),
                    path: Some(tempdir.path().join("ffx-orelse")),
                    ..basic_subtool_definition
                },
            ]
            .into_iter(),
        );
        let found_commands = HashSet::from_iter(
            find_workspace_tools(&[tempdir.path()]).filter_map(|tool| tool.validate_tool()),
        );
        assert_eq!(found_commands, expected_commands, "subtools we created should exist");

        suite
            .try_from_args(&FfxCommandLine {
                command: vec!["ffx".to_owned()],
                ffx_args: vec![],
                global: Ffx { subcommand: vec!["whatever".to_owned()], ..Default::default() },
            })
            .await
            .expect("should be able to find mock subtool in suite");
    }

    #[test]
    fn subtool_config_none() {
        assert!(get_subtool_paths(vec![]).is_empty());
    }

    #[test]
    fn subtool_config_one() {
        assert_eq!(get_subtool_paths(vec![json!("boom")]), vec![PathBuf::from("boom")]);
    }

    #[test]
    fn subtool_config_multiple() {
        assert_eq!(
            get_subtool_paths(vec![json!("boom"), json!("zoom")]),
            vec![PathBuf::from("boom"), PathBuf::from("zoom")]
        );
    }

    #[test]
    fn subtool_config_listlist() {
        assert_eq!(
            get_subtool_paths(vec![json!(["boom", "zoom"])]),
            vec![PathBuf::from("boom"), PathBuf::from("zoom")]
        );
    }

    #[test]
    fn subtool_config_multiple_listlist() {
        assert_eq!(
            get_subtool_paths(vec![json!(["boom", "zoom"]), json!(["doom", "loom"])]),
            vec![
                PathBuf::from("boom"),
                PathBuf::from("zoom"),
                PathBuf::from("doom"),
                PathBuf::from("loom")
            ]
        );
    }

    #[test]
    fn subtool_config_multiple_different() {
        assert_eq!(
            get_subtool_paths(vec![json!("boom"), json!(["doom", "loom"])]),
            vec![PathBuf::from("boom"), PathBuf::from("doom"), PathBuf::from("loom")]
        );
        assert_eq!(
            get_subtool_paths(vec![json!(["boom", "zoom"]), json!("loom")]),
            vec![PathBuf::from("boom"), PathBuf::from("zoom"), PathBuf::from("loom")]
        );
    }
}
