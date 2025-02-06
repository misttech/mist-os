// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Package ffxutil provides support for running ffx commands.
package ffxutil

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"go.fuchsia.dev/fuchsia/tools/bootserver"
	botanistconstants "go.fuchsia.dev/fuchsia/tools/botanist/constants"
	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/lib/ffxutil/constants"
	"go.fuchsia.dev/fuchsia/tools/lib/jsonutil"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/lib/retry"
	"go.fuchsia.dev/fuchsia/tools/lib/subprocess"
)

const (
	// The name of the snapshot zip file that gets outputted by `ffx target snapshot`.
	// Keep in sync with //src/developer/ffx/plugins/target/snapshot/src/lib.rs.
	snapshotZipName = "snapshot.zip"

	// The environment variable that ffx uses to create an isolated instance.
	FFXIsolateDirEnvKey = "FFX_ISOLATE_DIR"

	// The name of the ffx env config file.
	ffxEnvFilename = ".ffx_env"
)

type LogLevel string

const (
	Off   LogLevel = "Off"
	Error LogLevel = "Error"
	Warn  LogLevel = "Warn"
	Info  LogLevel = "Info"
	Debug LogLevel = "Debug"
	Trace LogLevel = "Trace"
)

type ffxCmdBuilder interface {
	// Build an ffx command with appropriate additional arguments
	command(ffxPath string, args []string) []string
	// Store the configuration for future ffx invocations
	setConfig(user, global map[string]any) error
	// Return additional environment variables required by this runner
	env() []string
}

// stdCmdFfxBuilder is a builder for ffx commands using the 'standard" approach:
// with isolate dirs, invoking `ffx config set` to specify configurations,
// etc. Eventually we will also have a "strict" builder which will abide by
// ffx-strict semantics (see b/391391857).
type stdFfxCmdBuilder struct {
	isolateDir string
	configDir  string
}

func newStdFfxCmdBuilder(
	isolateDir string,
	configDir string,
) *stdFfxCmdBuilder {
	return &stdFfxCmdBuilder{
		isolateDir,
		configDir,
	}
}

func (r *stdFfxCmdBuilder) command(ffxPath string, args []string) []string {
	return append([]string{ffxPath, "--isolate-dir", r.isolateDir}, args...)
}

func (r *stdFfxCmdBuilder) setConfig(user, global map[string]any) error {
	ffxEnvFilepath := filepath.Join(r.configDir, ffxEnvFilename)
	globalConfigFilepath := filepath.Join(r.configDir, "global_config.json")
	userConfigFilepath := filepath.Join(r.configDir, "user_config.json")
	ffxEnvSettings := map[string]any{
		"user":   userConfigFilepath,
		"global": globalConfigFilepath,
	}
	if err := writeConfigFile(globalConfigFilepath, global); err != nil {
		return fmt.Errorf("failed to write ffx global config at %s: %w", globalConfigFilepath, err)
	}
	if err := writeConfigFile(userConfigFilepath, user); err != nil {
		return fmt.Errorf("failed to write ffx user config at %s: %w", userConfigFilepath, err)
	}
	if err := writeConfigFile(ffxEnvFilepath, ffxEnvSettings); err != nil {
		return fmt.Errorf("failed to write ffx env file at %s: %w", ffxEnvFilepath, err)
	}
	return nil
}

func (r *stdFfxCmdBuilder) env() []string {
	return []string{fmt.Sprintf("%s=%s", FFXIsolateDirEnvKey, r.isolateDir)}
}

// FFXInstance takes in a path to the ffx tool and runs ffx commands with the provided config.
type FFXInstance struct {
	ctx     context.Context
	ffxPath string

	runner     *subprocess.Runner
	cmdBuilder ffxCmdBuilder
	stdout     io.Writer
	stderr     io.Writer
	target     string
	env        []string
}

// ConfigSettings contains settings to apply to the ffx configs at the specified config level.
type ConfigSettings struct {
	Level    string
	Settings map[string]any
}

// Output for get-artifacts
type ProductArtifacts struct {
	Ok struct {
		Paths []string `json:"paths"`
	} `json:"ok"`
	UserError struct {
		Message string `json:"message"`
	} `json:"user_error"`
	UnexpectedError struct {
		Message string `json:"message"`
	} `json:"unexpected_error"`
}

// If ffx outputs machine errors then this is the schema it is output in
type FfxMachineError struct {
	Type    string `json:"type"`
	Code    int    `json:"code"`
	Message string `json:"message"`
}

func (e *FfxMachineError) Error() string {
	return fmt.Sprintf("Error type \"%s\" with code %v, and message  \"\"%s",
		e.Type, e.Code, e.Message)
}

// Output for get-image-path
type ProductImagePath struct {
	Ok struct {
		Path string `json:"path"`
	} `json:"ok"`
	UserError struct {
		Message string `json:"message"`
	} `json:"user_error"`
	UnexpectedError struct {
		Message string `json:"message"`
	} `json:"unexpected_error"`
}

// FFXWithTarget returns a copy of the provided ffx instance associated with
// the provided target. This copy should use the same ffx daemon but run
// commands with the new target.
func FFXWithTarget(ffx *FFXInstance, target string) *FFXInstance {
	return &FFXInstance{
		ctx:        ffx.ctx,
		ffxPath:    ffx.ffxPath,
		runner:     ffx.runner,
		cmdBuilder: ffx.cmdBuilder,
		stdout:     ffx.stdout,
		stderr:     ffx.stderr,
		target:     target,
		env:        ffx.env,
	}
}

// NewFFXInstance creates an isolated FFXInstance.
func NewFFXInstance(
	ctx context.Context,
	ffxPath string,
	processDir string,
	env []string,
	target, sshKey string,
	outputDir string,
	extraConfigSettings ...ConfigSettings,
) (*FFXInstance, error) {
	if ffxPath == "" {
		return nil, nil
	}
	absOutputDir, err := filepath.Abs(outputDir)
	if err != nil {
		return nil, err
	}
	if err := os.MkdirAll(absOutputDir, os.ModePerm); err != nil {
		return nil, err
	}

	absFFXPath, err := filepath.Abs(ffxPath)
	if err != nil {
		return nil, err
	}
	cmdBuilder := newStdFfxCmdBuilder(
		absOutputDir,
		absOutputDir,
	)
	env = append(os.Environ(), env...)
	env = append(env, cmdBuilder.env()...)
	ffx := &FFXInstance{
		ctx:        ctx,
		ffxPath:    absFFXPath,
		runner:     &subprocess.Runner{Dir: processDir, Env: env},
		cmdBuilder: cmdBuilder,
		stdout:     os.Stdout,
		stderr:     os.Stderr,
		target:     target,
		env:        env,
	}
	if sshKey != "" {
		sshKey, err = filepath.Abs(sshKey)
		if err != nil {
			return nil, err
		}
	}
	userConfig, globalConfig := buildConfigs(absOutputDir, absFFXPath, sshKey, extraConfigSettings)
	if err := cmdBuilder.setConfig(userConfig, globalConfig); err != nil {
		return nil, err
	}
	return ffx, nil
}

func buildConfigs(absOutputDir string, absFFXPath string, absSshKeyPath string, extraConfigSettings []ConfigSettings) (user, global map[string]any) {
	// Set these fields in the global config for tests that don't use this library
	// and don't set their own isolated env config.
	globalConfigSettings := map[string]any{
		// This is a config "alias" for various other config values -- disabling
		// metrics, device discovery, device auto-connection, etc.
		"ffx.isolated": true,
	}
	userConfigSettings := map[string]any{
		"log.dir":                      filepath.Join(absOutputDir, "ffx_logs"),
		"ffx.subtool-search-paths":     filepath.Dir(absFFXPath),
		"test.experimental_json_input": true,
	}
	if absSshKeyPath != "" {
		userConfigSettings["ssh.priv"] = []string{absSshKeyPath}
	}
	for _, settings := range extraConfigSettings {
		if settings.Level == "global" {
			for key, val := range settings.Settings {
				globalConfigSettings[key] = val
			}
		} else {
			for key, val := range settings.Settings {
				userConfigSettings[key] = val
			}
		}
	}
	if deviceAddr := os.Getenv(botanistconstants.DeviceAddrEnvKey); deviceAddr != "" {
		globalConfigSettings["discovery.mdns.enabled"] = false
	}
	return userConfigSettings, globalConfigSettings
}

func writeConfigFile(configPath string, configSettings map[string]any) error {
	data := make(map[string]any)
	for key, val := range configSettings {
		parts := strings.Split(key, ".")
		datakey := data
		for i, subkey := range parts {
			if i == len(parts)-1 {
				datakey[subkey] = val
			} else {
				if _, ok := datakey[subkey]; !ok {
					datakey[subkey] = make(map[string]any)
				}
				datakey = datakey[subkey].(map[string]any)
			}
		}
	}
	j, err := json.MarshalIndent(data, "", "  ")
	if err != nil {
		return fmt.Errorf("marshal ffx config: %w", err)
	}
	if err := os.WriteFile(configPath, j, 0o600); err != nil {
		return fmt.Errorf("writing ffx config to file: %w", err)
	}
	return nil
}

func (f *FFXInstance) Env() []string {
	return f.env
}

func (f *FFXInstance) SetTarget(target string) {
	f.target = target
}

func (f *FFXInstance) Stdout() io.Writer {
	return f.stdout
}

func (f *FFXInstance) Stderr() io.Writer {
	return f.stderr
}

// SetStdoutStderr sets the stdout and stderr for the ffx commands to write to.
func (f *FFXInstance) SetStdoutStderr(stdout, stderr io.Writer) {
	f.stdout = stdout
	f.stderr = stderr
}

// SetLogLevel sets the log-level in the ffx instance's associated config.
func (f *FFXInstance) SetLogLevel(ctx context.Context, level LogLevel) error {
	return f.ConfigSet(ctx, "log.level", string(level))
}

// ConfigSet sets a field in the ffx instance's associated config.
func (f *FFXInstance) ConfigSet(ctx context.Context, key, value string) error {
	return f.Run(ctx, "config", "set", key, value)
}

func (f *FFXInstance) invoker(args []string) *ffxInvoker {
	// By default, use the FFXInstance's stdout/stderr
	return &ffxInvoker{ffx: f, args: args, stdout: f.stdout, stderr: f.stderr}
}

type ffxInvoker struct {
	ffx           *FFXInstance
	args          []string
	target        string
	timeout       *time.Duration
	captureOutput bool
	output        *bytes.Buffer
	stdout        io.Writer
	stderr        io.Writer
}

func (f *ffxInvoker) cmd() *exec.Cmd {
	args := f.args
	if f.target != "" {
		args = append([]string{"--target", f.target}, args...)
	}
	ffx_cmd := f.ffx.cmdBuilder.command(f.ffx.ffxPath, args)
	return f.ffx.runner.Command(ffx_cmd, subprocess.RunOptions{
		Stdout: f.stdout,
		Stderr: f.stderr,
	})
}

// This function should only be invoked once in an ffxInvoker.
// The proper idiom is to construct the ffxInvoker, then immediately
// run (or use the `cmd()` method) without storing it
func (f *ffxInvoker) run(ctx context.Context) error {
	// By default, runs ffx commands with a 5 minute timeout.
	var timeout time.Duration = 5 * time.Minute
	if f.timeout != nil {
		timeout = *f.timeout
	}
	if timeout > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, timeout)
		defer cancel()
	}
	if f.captureOutput {
		// Store the output in our buffer
		var output bytes.Buffer
		f.output = &output
		stdout := io.MultiWriter(f.output, f.stdout)
		f.stdout = stdout
	}
	cmd := f.cmd()
	if err := f.ffx.runner.RunCommand(ctx, cmd); err != nil {
		return fmt.Errorf("%s (%s): %w", constants.CommandFailedMsg, cmd.String(), err)
	}
	return nil
}

func (i *ffxInvoker) setTarget(target string) *ffxInvoker {
	i.target = target
	return i
}

func (i *ffxInvoker) setTimeout(timeout time.Duration) *ffxInvoker {
	i.timeout = &timeout
	return i
}

func (i *ffxInvoker) setCaptureOutput() *ffxInvoker {
	i.captureOutput = true
	return i
}

func (i *ffxInvoker) setStdout(stdout io.Writer) *ffxInvoker {
	i.stdout = stdout
	return i
}

func (i *ffxInvoker) setStderr(stderr io.Writer) *ffxInvoker {
	i.stderr = stderr
	return i
}

// Convenience API

// Command returns an *exec.Cmd to run ffx with the provided args.
func (f *FFXInstance) Command(args ...string) *exec.Cmd {
	return f.invoker(args).cmd()
}

// CommandWithTarget returns a Command to run with the associated target.
func (f *FFXInstance) CommandWithTarget(args ...string) (*exec.Cmd, error) {
	if f.target == "" {
		return nil, fmt.Errorf("no target is set")
	}
	return f.invoker(args).setTarget(f.target).cmd(), nil
}

// RunWithTimeout runs ffx with the associated config and provided args.
func (f *FFXInstance) RunWithTimeout(ctx context.Context, timeout time.Duration, args ...string) error {
	return f.invoker(args).setTimeout(timeout).run(ctx)
}

// Run runs ffx with the associated config and provided args.
func (f *FFXInstance) Run(ctx context.Context, args ...string) error {
	return f.invoker(args).run(ctx)
}

// RunCommand runs the given cmd with the FFXInstance's subprocess runner.
func (f *FFXInstance) RunCommand(ctx context.Context, cmd *exec.Cmd) error {
	return f.runner.RunCommand(ctx, cmd)
}

// RunWithTarget runs ffx with the associated target.
func (f *FFXInstance) RunWithTarget(ctx context.Context, args ...string) error {
	if f.target == "" {
		return fmt.Errorf("no target is set")
	}
	return f.invoker(args).setTarget(f.target).run(ctx)
}

// RunWithTargetAndTimeout runs ffx with the associated target and timeout.
func (f *FFXInstance) RunWithTargetAndTimeout(ctx context.Context, timeout time.Duration, args ...string) error {
	if f.target == "" {
		return fmt.Errorf("no target is set")
	}
	return f.invoker(args).setTarget(f.target).setTimeout(timeout).run(ctx)
}

// RunAndGetOutput runs ffx with the provided args and returns the stdout.
func (f *FFXInstance) RunAndGetOutput(ctx context.Context, args ...string) (string, error) {
	i := f.invoker(args).setCaptureOutput()
	err := i.run(ctx)
	s := i.output.String()
	return strings.TrimSpace(s), err
}

// WaitForDaemon tries a few times to check that the daemon is up
// and returns an error if it fails to respond.
func (f *FFXInstance) WaitForDaemon(ctx context.Context) error {
	// Discard the stderr since it'll return a string caught by
	// tefmocheck if the daemon isn't ready yet.
	origStderr := f.stderr
	var output bytes.Buffer
	f.stderr = &output
	defer func() {
		f.stderr = origStderr
	}()
	// Normally trying 10 times would be overkill, but we know that the arm64 emulator sometimes
	// has delays (b/330228364), so let's keep trying for 10 seconds instead of just 3, in order
	// to address an occasional failure when the daemon doesn't respond quickly (b/316626057)
	err := retry.Retry(ctx, retry.WithMaxAttempts(retry.NewConstantBackoff(time.Second), 10), func() error {
		return f.RunWithTimeout(ctx, 0, "daemon", "echo")
	}, nil)
	if err != nil {
		logger.Warningf(ctx, "failed to echo daemon: %s", output.String())
	}
	return err
}

// Stop stops the daemon.
func (f *FFXInstance) Stop() error {
	// Wait up to 4000ms for daemon to shut down.
	return f.Run(context.Background(), "daemon", "stop", "-t", "4000")
}

// BootloaderBoot RAM boots the target.
func (f *FFXInstance) BootloaderBoot(ctx context.Context, target, productBundle string) error {
	args := []string{
		"--target", target,
		"--config", "{\"ffx\": {\"fastboot\": {\"inline_target\": true}}}",
		"target", "bootloader",
	}
	args = append(args, "--product-bundle", productBundle)
	args = append(args, "boot")
	return f.RunWithTimeout(ctx, 0, args...)
}

// List lists all available targets.
func (f *FFXInstance) List(ctx context.Context, args ...string) error {
	return f.Run(ctx, append([]string{"target", "list"}, args...)...)
}

// TargetWait waits until the target becomes available.
func (f *FFXInstance) TargetWait(ctx context.Context) error {
	return f.RunWithTarget(ctx, "target", "wait")
}

// Test runs a test suite.
func (f *FFXInstance) Test(
	ctx context.Context,
	testList build.TestList,
	outDir string,
	args ...string,
) (*TestRunResult, error) {
	// Write the test def to a file and store in the outDir to upload with the test outputs.
	if err := os.MkdirAll(outDir, os.ModePerm); err != nil {
		return nil, err
	}
	testFile := filepath.Join(outDir, "test-list.json")
	if err := jsonutil.WriteToFile(testFile, testList); err != nil {
		return nil, err
	}
	// Create a new subdirectory within outDir to pass to --output-directory which is expected to be
	// empty.
	testOutputDir := filepath.Join(outDir, "test-outputs")
	f.RunWithTargetAndTimeout(
		ctx,
		0,
		append(
			[]string{
				"test",
				"run",
				"--continue-on-timeout",
				"--test-file",
				testFile,
				"--output-directory",
				testOutputDir,
				"--show-full-moniker-in-logs",
			},
			args...)...)

	return GetRunResult(testOutputDir)
}

// Snapshot takes a snapshot of the target's state and saves it to outDir/snapshotFilename.
func (f *FFXInstance) Snapshot(ctx context.Context, outDir string, snapshotFilename string) error {
	err := f.RunWithTarget(ctx, "target", "snapshot", "--dir", outDir)
	if err != nil {
		return err
	}
	if snapshotFilename != "" && snapshotFilename != snapshotZipName {
		return os.Rename(
			filepath.Join(outDir, snapshotZipName),
			filepath.Join(outDir, snapshotFilename),
		)
	}
	return nil
}

// GetConfig shows the ffx config.
func (f *FFXInstance) GetConfig(ctx context.Context) error {
	return f.Run(ctx, "config", "get")
}

// GetSshPrivateKey returns the file path for the ssh private key.
func (f *FFXInstance) GetSshPrivateKey(ctx context.Context) (string, error) {
	// Check that the keys exist and are valid
	if err := f.Run(ctx, "config", "check-ssh-keys"); err != nil {
		return "", err
	}
	key, err := f.RunAndGetOutput(ctx, "config", "get", "ssh.priv")
	if err != nil {
		return "", err
	}

	// strip quotes if present.
	key = strings.Replace(key, "\"", "", -1)
	return key, nil
}

// GetSshAuthorizedKeys returns the file path for the ssh auth keys.
func (f *FFXInstance) GetSshAuthorizedKeys(ctx context.Context) (string, error) {
	// Check that the keys exist and are valid
	if err := f.Run(ctx, "config", "check-ssh-keys"); err != nil {
		return "", err
	}
	key, err := f.RunAndGetOutput(ctx, "config", "get", "ssh.pub")
	if err != nil {
		return "", err
	}

	// strip quotes if present.
	key = strings.Replace(key, "\"", "", -1)
	return key, nil
}

// GetPBArtifacts returns a list of the artifacts required for the specified artifactsGroup (flash or emu).
// The returned list are relative paths to the pbPath.
func (f *FFXInstance) GetPBArtifacts(ctx context.Context, pbPath string, artifactsGroup string) ([]string, error) {
	raw, err := f.RunAndGetOutput(ctx, "--machine", "json", "product", "get-artifacts", pbPath, "-r", "-g", artifactsGroup)

	return processPBArtifactsResult(raw, err)
}

func processPBArtifactsResult(raw string, err error) ([]string, error) {

	if raw == "" {
		if err != nil {
			return nil, err
		}
		return nil, fmt.Errorf("No output received from command")

	}

	// If there was an error, parse the output first since it will be a stable interface, otherwise fall back.
	var result ProductArtifacts
	err = json.Unmarshal([]byte(raw), &result)
	if err != nil {
		return nil, fmt.Errorf("Error parsing output %s: %v", raw, err)
	}
	if result.UnexpectedError.Message != "" {
		return nil, fmt.Errorf("unexpected error: %s", result.UnexpectedError.Message)
	}
	if result.UserError.Message != "" {
		return nil, fmt.Errorf("user error: %s", result.UserError.Message)
	}
	return result.Ok.Paths, nil
}

// GetImageFromPB returns an image from a product bundle.
func (f *FFXInstance) GetImageFromPB(ctx context.Context, pbPath string, slot string, imageType string, bootloader string) (*bootserver.Image, error) {
	args := []string{"--machine", "json", "product", "get-image-path", pbPath, "-r"}
	if slot != "" && imageType != "" && bootloader == "" {
		args = append(args, "--slot", slot, "--image-type", imageType)
	} else if bootloader != "" && slot == "" && imageType == "" {
		args = append(args, "--bootloader", bootloader)
	} else {
		return nil, fmt.Errorf("either slot and image type should be provided or bootloader "+
			"should be provided, not both: slot: %s, imageType: %s, bootloader: %s", slot, imageType, bootloader)
	}
	raw, err := f.RunAndGetOutput(ctx, args...)

	return processImageFromPBResult(pbPath, raw, err)
}

func processImageFromPBResult(pbPath string, raw string, err error) (*bootserver.Image, error) {
	if raw == "" {
		if err != nil {
			return nil, err
		}
		return nil, fmt.Errorf("No output received from command")

	} else if err != nil {
		// We got output and there is an error. Try to find it.
		// We'll only take the last one
		dec := json.NewDecoder(strings.NewReader(raw))
		var errMsg FfxMachineError
		for {
			if marshalErr := dec.Decode(&errMsg); marshalErr == io.EOF {
				// Cool we got the message
				break
			} else if marshalErr != nil {
				return nil, fmt.Errorf("Error parsing error message %s: %v", raw, marshalErr)
			}
		}

		if errMsg.Type == "unexpected" || errMsg.Type == "user" {
			// An error is returned if the image cannot be found in the product bundle
			// which is ok.
			return nil, nil
		}

		return nil, fmt.Errorf("Error getting image: %v", errMsg)
	}

	var result ProductImagePath
	err = json.Unmarshal([]byte(raw), &result)
	if err != nil {
		return nil, fmt.Errorf("Error parsing output %s: %v", raw, err)
	}

	if result.UnexpectedError.Message != "" || result.UserError.Message != "" {
		// An error is returned if the image cannot be found in the product bundle
		// which is ok.
		return nil, nil
	}

	imagePath := filepath.Join(pbPath, result.Ok.Path)
	buildImg := build.Image{Name: result.Ok.Path, Path: imagePath}
	reader, err := os.Open(imagePath)
	if err != nil {
		return nil, err
	}
	fi, err := reader.Stat()
	if err != nil {
		return nil, err
	}
	image := bootserver.Image{
		Image:  buildImg,
		Reader: reader,
		Size:   fi.Size(),
	}

	return &image, nil
}
