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
	"slices"
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

type FFXInvokeMode int

const (
	UseFFXLegacy FFXInvokeMode = iota
	UseFFXStrict
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
	command(ffxPath string, supportsStrict bool, args []string) []string
	// Build an ffx command with appropriate additional arguments and specific config values
	commandWithConfigs(ffxPath string, supportsStrict bool, args []string, configs map[string]any) []string
	// Store the configuration for future ffx invocations
	setConfigMap(user, global map[string]any) error
	// Get the config map
	getConfigMap() map[string]any
	// Add an individual mapping to the configuration
	setConfig(key, val string)
	// Return additional environment variables required by this runner
	env() []string
	// Return whether this builder is strict
	isStrict() bool
}

// legacyFfxCmdBuilder is a builder for ffx commands using the "legacy" approach:
// with isolate dirs, invoking `ffx config set` to specify configurations,
// etc. See strictFfxCmdBuilder for the alternate mechanism.
type legacyFfxCmdBuilder struct {
	isolateDir string
	configDir  string
}

func newLegacyFfxCmdBuilder(
	isolateDir string,
	configDir string,
) *legacyFfxCmdBuilder {
	return &legacyFfxCmdBuilder{
		isolateDir,
		configDir,
	}
}

func (b *legacyFfxCmdBuilder) command(ffxPath string, supportsStrict bool, args []string) []string {
	return b.commandWithConfigs(ffxPath, supportsStrict, args, map[string]any{})
}

func (b *legacyFfxCmdBuilder) commandWithConfigs(ffxPath string, _supportsStrict bool, args []string, configs map[string]any) []string {
	configArgs := []string{}
	for key, val := range configs {
		configArgs = append(configArgs, []string{"-c", fmt.Sprintf("%s=%v", key, val)}...)
	}
	args = append(configArgs, args...)
	return append([]string{ffxPath, "--isolate-dir", b.isolateDir}, args...)
}

func (b *legacyFfxCmdBuilder) setConfigMap(user, global map[string]any) error {
	ffxEnvFilepath := filepath.Join(b.configDir, ffxEnvFilename)
	globalConfigFilepath := filepath.Join(b.configDir, "global_config.json")
	userConfigFilepath := filepath.Join(b.configDir, "user_config.json")
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

// This should not be called. (Having this code run "ffx config get") would
// require invoking an external program, which is not the responsibility of
// an ffxCmdBuilder)
func (b *legacyFfxCmdBuilder) getConfigMap() map[string]any {
	panic("getConfigMap shouldn't be called with legacy ffx")
}

// This should not be called. (Having this code run "ffx config set") would
// require invoking an external program, which is not the responsibility of
// an ffxCmdBuilder)
func (b *legacyFfxCmdBuilder) setConfig(key, val string) {
	panic("setConfig shouldn't be called with legacy ffx")
}

func (b *legacyFfxCmdBuilder) env() []string {
	return []string{fmt.Sprintf("%s=%s", FFXIsolateDirEnvKey, b.isolateDir)}
}

func (f *legacyFfxCmdBuilder) isStrict() bool {
	return false
}

// strictFfxCmdBuilder is a builder for ffx commands using the 'strict"
// approach: no isolate dir, all required configs passed to the command,
// etc.
type strictFfxCmdBuilder struct {
	outputFile    string
	configs       map[string]any
	tmpIsolateDir string
}

func newStrictFfxCmdBuilder(outputFile string) *strictFfxCmdBuilder {
	return &strictFfxCmdBuilder{outputFile: outputFile}
}

// This is a _temporary_ mechanism for using strict ffx, while the
// necessary subtools are still being developed. It must be provided
// until the ffx-strict transition is complete, but it is not part of
// the constructor, as a hint that this is mechanism is temporary.
// TODO(slgrady): Remove this function and all references to isolateDir
// from strictFfxCmdBuilder once all necessary ffx subtools implement
// "--strict"
func (b *strictFfxCmdBuilder) tempSetIsolateDir(dir string) {
	b.tmpIsolateDir = dir
}

// Build the required command for a strict invocation. Note that the command
// may require the target, but the caller should have set that if so.
func (b *strictFfxCmdBuilder) command(ffxPath string, supportsStrict bool, args []string) []string {
	return b.commandWithConfigs(ffxPath, supportsStrict, args, map[string]any{})
}

// Build the required command for a strict invocation, including any invocation-specific config options.
// Note that the command may require the target, but the caller should have set that if so.
func (b *strictFfxCmdBuilder) commandWithConfigs(ffxPath string, supportsStrict bool, args []string, configs map[string]any) []string {
	cmd := []string{ffxPath, "-o", b.outputFile}

	if supportsStrict {
		// The command does in fact support --strict, so let's use it!
		cmd = append(cmd, "--strict")
	} else {
		// We don't support strict, so we'd better keep using the isolate dir
		cmd = append(cmd, "--isolate-dir", b.tmpIsolateDir)
	}

	// Commands that support strict always need a machine argument. If none is supplied,
	// default to "json".
	if !slices.Contains(args, "--machine") && supportsStrict {
		cmd = append(cmd, "--machine", "json")
	}

	// TODO(slgrady): Eventually use the specific configs required for
	// particular commands. Unfortunately that information is not
	// available from ffx yet.
	for k, v := range b.configs {
		// The caller may have already provided the config
		if _, exists := configs[k]; !exists {
			cmd = append(cmd, "-c", fmt.Sprintf("%s=%v", k, v))
		}

	}

	for k, v := range configs {
		cmd = append(cmd, "-c", fmt.Sprintf("%s=%v", k, v))
	}
	return append(cmd, args...)
}

// Store the configuration for future invocations
// TODO(slgrady): change this function to take a single map of
// configuration options once we remove non-strict ffx invocations,
func (b *strictFfxCmdBuilder) setConfigMap(user, global map[string]any) error {
	// In strict mode, both user and global configurations are passed on the
	// command line, so there is no need to differentiate them, so we just merge
	// them here before saving them.
	// Overwrite global with user, in case there are duplicates. This
	// matches the priority of ffx config.
	for k, v := range user {
		global[k] = v
	}

	// Strip out certain configs that don't apply to strict invocations
	// TODO(slgrady): enable these once we support all strict commands, and
	// so none of these configs are necessary. (Until then, we need
	// to be careful, continuing to prevent a "strict" invocation
	// from escaping its isolation)
	// (And later still: TODO(slgrady): remove this code once we remove
	//  non-strict ffx invocations)
	// delete(global, "daemon.autostart")
	// delete(global, "ffx.target-list.local-connect")

	b.configs = global
	return nil
}

func (b *strictFfxCmdBuilder) getConfigMap() map[string]any {
	return b.configs
}

func (b *strictFfxCmdBuilder) setConfig(key, val string) {
	b.configs[key] = val
}

// TODO(slgrady): remove this method once we get rid of isolate dirs, when
// strict is fully supported
func (b *strictFfxCmdBuilder) env() []string {
	return []string{fmt.Sprintf("%s=%s", FFXIsolateDirEnvKey, b.tmpIsolateDir)}
}

func (f *strictFfxCmdBuilder) isStrict() bool {
	return true
}

// FFXInstance takes in a path to the ffx tool and runs ffx commands with the provided config.
type FFXInstance struct {
	ctx     context.Context
	ffxPath string

	runner         *subprocess.Runner
	cmdBuilder     ffxCmdBuilder
	stdout         io.Writer
	stderr         io.Writer
	target         string
	env            []string
	sshInfo        SSHInfo
	sshKeysChecked bool
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

// SSHInfo contains the pathnames of private and public keys. The requirements
// on the fields depened depend on the invoke-mode. When in strict, SshPriv
// must be specified, but SshPub can be empty. When in legacy mode, if either
// field is empty, the corresponding path will be inferred from ffx config.
type SSHInfo struct {
	SshPriv string
	SshPub  string
}

// NewFFXInstance creates an isolated FFXInstance.
func NewFFXInstance(
	ctx context.Context,
	ffxPath string,
	processDir string,
	env []string,
	target string,
	sshInfo *SSHInfo,
	outputDir string,
	invokeMode FFXInvokeMode,
	extraConfigSettings ...ConfigSettings,
) (*FFXInstance, error) {
	logger.Debugf(ctx, "NewFFXInstance: ffx=%s dir=%s target=%s sshInfo=%v oDir=%s", ffxPath, processDir, target, sshInfo, outputDir)
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
	var cmdBuilder ffxCmdBuilder
	if invokeMode == UseFFXStrict {
		outputFile := filepath.Join(absOutputDir, "ffx.log")
		var strictFfx = newStrictFfxCmdBuilder(outputFile)
		// TODO(slgrady): Remove the isolateDir when ffx-strict is fully
		// implemented
		strictFfx.tempSetIsolateDir(absOutputDir)
		cmdBuilder = strictFfx
	} else {
		cmdBuilder = newLegacyFfxCmdBuilder(
			absOutputDir,
			absOutputDir,
		)

	}
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
	// Always make sure we have the ssh keys
	sshInfo, err = getOrBuildSshKeys(ctx, invokeMode, sshInfo, ffx)
	if err != nil {
		return nil, err
	}
	sshPriv, err := filepath.Abs(sshInfo.SshPriv)
	if err != nil {
		return nil, err
	}
	sshPub, err := filepath.Abs(sshInfo.SshPub)
	if err != nil {
		return nil, err
	}
	// Cache the ssh keys
	ffx.sshInfo = SSHInfo{SshPriv: sshPriv, SshPub: sshPub}
	userConfig, globalConfig := buildConfigs(absOutputDir, absFFXPath, ffx.sshInfo, extraConfigSettings)
	if err := cmdBuilder.setConfigMap(userConfig, globalConfig); err != nil {
		return nil, err
	}
	return ffx, nil
}

// Make sure we have valid ssh keys. The specific logic depends on whether the
// client has the keys to us or not, as well as whether we were invoked in
// "strict" mode.
func getOrBuildSshKeys(ctx context.Context, invokeMode FFXInvokeMode, sshInfo *SSHInfo, ffx *FFXInstance) (*SSHInfo, error) {
	if invokeMode != UseFFXStrict {
		// In legacy mode, we can build the keys ourselves, or the caller can provide
		// the private key, or both keys
		if sshInfo == nil {
			envInfo, err := ffx.getSshKeysFromEnvironment(ctx)
			if err != nil {
				return nil, err
			}
			sshInfo = &envInfo
		} else {
			if sshInfo.SshPriv == "" {
				privPath, err := ffx.getSshKeyFromEnvironment(ctx, "ssh.priv")
				if err != nil {
					return nil, fmt.Errorf("Couldn't get ssh private key from config: %w", err)
				}
				sshInfo.SshPriv = privPath
			}
			if sshInfo.SshPub == "" {
				pubPath, err := ffx.getSshKeyFromEnvironment(ctx, "ssh.pub")
				if pubPath == "" {
					pubPath = sshInfo.SshPriv + ".pub"
					logger.Debugf(ctx, "No ssh public key (%s), using %s", err, pubPath)
				}
				sshInfo.SshPub = pubPath
			}
		}
	} else {
		// In strict mode, the caller must provide the private key, and if they don't provide the public
		// key we'll assume it's <private> + ".pub"
		if sshInfo == nil {
			return nil, fmt.Errorf("SSH Key information must be provided when in strict mode")
		}
		if sshInfo.SshPriv == "" {
			return nil, fmt.Errorf("SSH Private Key information must be provided when in strict mode")
		}
		if sshInfo.SshPub == "" {
			sshInfo.SshPub = sshInfo.SshPriv + ".pub"
		}
	}
	// Make sure the keys exist and match
	if err := ffx.checkSpecificSshKeys(ctx, sshInfo.SshPriv, sshInfo.SshPub); err != nil {
		return nil, err
	}
	return sshInfo, nil
}

func buildConfigs(absOutputDir string, absFFXPath string, sshInfo SSHInfo, extraConfigSettings []ConfigSettings) (user, global map[string]any) {
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
		"emu.instance_dir":             filepath.Join(absOutputDir, "emu/instances"),
		"ssh.priv":                     sshInfo.SshPriv,
		"ssh.pub":                      sshInfo.SshPub,
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

func (f *FFXInstance) isStrict() bool {
	return f.cmdBuilder.isStrict()
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

func (f *FFXInstance) ConfigEnv(ctx context.Context) error {
	// Irrelevant in strict mode
	if f.isStrict() {
		fmt.Fprintln(f.stdout, "ffx config env unnecessary with ffx-strict")
		return nil
	}
	return f.Run(ctx, "config", "env")
}

// ConfigSet sets a field in the ffx instance's associated config.
func (f *FFXInstance) ConfigSet(ctx context.Context, key, value string) error {
	// This code is ugly, but we can't delegate the setting to the ffxCmdBuilder
	// since we may need to invoke an external program, and adding that responsibility
	// to the ffxCmdBuilder would be ugly. This code will all go away once we switch
	// fully over to ffx-strict
	if !f.isStrict() {
		return f.Run(ctx, "config", "set", key, value)
	}
	f.cmdBuilder.setConfig(key, value)
	return nil
}

type MachineFormat int

const (
	MachineJson MachineFormat = iota + 1
	MachineJsonPretty
	MachineRaw
	MachineNone
)

func (mf MachineFormat) str() string {
	if mf == MachineJson {
		return "json"
	} else if mf == MachineJsonPretty {
		return "json-pretty"
	} else if mf == MachineRaw {
		return "raw"
	} else {
		return "none"
	}
}

func (f *FFXInstance) invoker(args []string) *ffxInvoker {
	// By default, use the FFXInstance's stdout/stderr
	return &ffxInvoker{ffx: f, args: args, stdout: f.stdout, stderr: f.stderr, machineFormat: MachineNone}
}

type ffxInvoker struct {
	ffx            *FFXInstance
	args           []string
	target         string
	timeout        *time.Duration
	captureOutput  bool
	output         *bytes.Buffer
	stdout         io.Writer
	stderr         io.Writer
	machineFormat  MachineFormat
	configs        map[string]any
	supportsStrict bool // TODO(slgrady) Remove once all required commands support --strict
}

func (f *ffxInvoker) cmd() *exec.Cmd {
	args := f.args
	if f.target != "" {
		args = append([]string{"--target", f.target}, args...)
	}
	// Priority logic for setting machine format:
	// 1: if command includes "--machine", use it
	// 2: instead, if invoker sets the machine format, use that
	// 3: otherwise, cmdBuilder may add --machine json (only if supportsStrict).
	if !slices.Contains(args, "--machine") {
		if f.machineFormat != MachineNone {
			args = append([]string{"--machine", f.machineFormat.str()}, args...)
		}
	}
	ffx_cmd := f.ffx.cmdBuilder.commandWithConfigs(f.ffx.ffxPath, f.supportsStrict, args, f.configs)

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

func (i *ffxInvoker) setStrict() *ffxInvoker {
	i.supportsStrict = true
	return i
}

func (i *ffxInvoker) setConfigs(configs map[string]any) *ffxInvoker {
	i.configs = configs
	return i
}

// Command to set the format. It defaults to MachineNone (no machine format).
// Strict commands always require a format, but the format can be
// MachineRaw, which means the caller is accepting the output in
// whatever format the ffx subtool provides. The strict cmdBuilder
// will add "--machine json" if no format has been specified.
func (i *ffxInvoker) setMachineFormat(format MachineFormat) *ffxInvoker {
	i.machineFormat = format
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

func (f *FFXInstance) StartDaemon(ctx context.Context, daemonLog *os.File) *exec.Cmd {
	args := []string{"daemon", "start"}
	// Special-case the daemon handling, since it's the one command where we want to redirect the stdout,
	// _instead_ of specifying "-o ffx.log"
	invoker := f.invoker(args)
	ffx_cmd := invoker.ffx.cmdBuilder.command(f.ffxPath, false, args)
	// Strip out the "--machine json" and "-o file" flags
	for i, arg := range ffx_cmd {
		if arg == "--machine" || arg == "-o" {
			// Note: i+2 because we want to remove both "--machine"
			// and the following arg
			ffx_cmd = append(ffx_cmd[:i], ffx_cmd[i+2:]...)
		}
	}

	cmd := f.runner.Command(ffx_cmd, subprocess.RunOptions{
		Stdout: daemonLog,
		Stderr: f.stderr,
	})
	logger.Debugf(ctx, "%s", cmd.Args)
	return cmd
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
		// "ffx daemon echo" _does_ support "--machine json", but when it is specified, the error comes
		// on stdout, so tefmo catches it despite the redirection of stderr. To preserve the redirection,
		// we'll tell the invoker not to use "--machine".
		return f.invoker([]string{"daemon", "echo"}).setTimeout(0).setMachineFormat(MachineNone).run(ctx)
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
func (f *FFXInstance) BootloaderBoot(ctx context.Context, target, productBundle string, tcpFlash bool) error {
	logger.Infof(ctx, "running bootloader boot with tcp flash: %t ", tcpFlash)
	configs := map[string]any{
		"discovery.mdns.enabled": false,
		"fastboot.usb.disabled":  true,
		"discovery.timeout":      12000,
	}

	if !tcpFlash {
		configs["ffx.fastboot.inline_target"] = true
	}

	return f.invoker([]string{"target", "bootloader", "--product-bundle", productBundle, "boot"}).setTarget(target).setStrict().setTimeout(0).setConfigs(configs).run(ctx)
}

// TestEarlyBootProfile puts a target's early boot profile in the specified directory
func (f *FFXInstance) TestEarlyBootProfile(ctx context.Context, outputDirectory string) error {
	if f.target == "" {
		return fmt.Errorf("no target is set")
	}
	return f.invoker([]string{"test", "early-boot-profile", "--output-directory", outputDirectory}).setTarget(f.target).setStrict().setTimeout(0).run(ctx)
}

// List lists all available targets.
func (f *FFXInstance) List(ctx context.Context, args ...string) error {
	return f.Run(ctx, append([]string{"target", "list"}, args...)...)
}

// TargetWait waits until the target becomes available.
func (f *FFXInstance) TargetWait(ctx context.Context, args ...string) error {
	if f.target == "" {
		return fmt.Errorf("no target is set")
	}
	return f.invoker(append([]string{"target", "wait"}, args...)).setTarget(f.target).setStrict().run(ctx)
}

// EmuStart returns an invoker to start the emulator with the specified
// commands.
func (f *FFXInstance) EmuStart(args ...string) *ffxInvoker {
	return f.invoker(append([]string{"emu", "start"}, args...)).setStrict()
}

func (f *FFXInstance) EmuStop(ctx context.Context, args ...string) error {
	return f.invoker(append([]string{"emu", "stop"}, args...)).setStrict().run(ctx)
}

// TestRun runs a test suite.
func (f *FFXInstance) TestRun(
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
	args = append(
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
		args...)
	if err := f.invoker(args).setTarget(f.target).setTimeout(0).setStrict().setMachineFormat(MachineRaw).run(ctx); err != nil {
		return nil, err
	}

	return GetRunResult(testOutputDir)
}

// Snapshot takes a snapshot of the target's state and saves it to outDir/snapshotFilename.
func (f *FFXInstance) Snapshot(ctx context.Context, outDir string, snapshotFilename string) error {
	if f.target == "" {
		return fmt.Errorf("no target is set")
	}
	err := f.invoker([]string{"target", "snapshot", "--dir", outDir}).setStrict().setTarget(f.target).run(ctx)
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
	if f.isStrict() {
		for k, v := range f.cmdBuilder.getConfigMap() {
			fmt.Fprintf(f.stdout, "ffx config: {%s}={%v}\n", k, v)
		}
		return nil
	} else {
		return f.Run(ctx, "config", "get")
	}
}

func (f *FFXInstance) checkSpecificSshKeys(ctx context.Context, privPath, pubPath string) error {
	// Check that the keys exist and are valid
	cfgs := fmt.Sprintf("ssh.priv=%s,ssh.pub=%s", privPath, pubPath)
	args := []string{"-c", cfgs, "config", "check-ssh-keys"}
	if err := f.invoker(args).setStrict().run(ctx); err != nil {
		return err
	}
	return nil
}

// GetSshPrivateKey returns the file path for the ssh private key.
func (f *FFXInstance) GetSshPrivateKey() string {
	return f.sshInfo.SshPriv
}

// GetSshAuthorizedKeys returns the file path for the ssh auth keys.
func (f *FFXInstance) GetSshAuthorizedKeys() string {
	return f.sshInfo.SshPub
}

func (f *FFXInstance) getSshKeyFromEnvironment(ctx context.Context, key string) (string, error) {
	args := []string{"config", "get", key}
	// TODO(slgrady): when `ffx --machine json config get` works, change to MachineJson, and parse
	// the output
	i := f.invoker(args).setCaptureOutput().setMachineFormat(MachineRaw)
	err := i.run(ctx)
	if err != nil {
		return "", err
	}
	val := i.output.String()

	// strip quotes if present.
	val = strings.Replace(strings.TrimSpace(val), "\"", "", -1)
	return val, nil
}

func (f *FFXInstance) getSshKeysFromEnvironment(ctx context.Context) (SSHInfo, error) {
	sshPriv, err := f.getSshKeyFromEnvironment(ctx, "ssh.priv")
	if err != nil {
		return SSHInfo{}, err
	}
	sshPub, err := f.getSshKeyFromEnvironment(ctx, "ssh.pub")
	if err != nil {
		return SSHInfo{}, err
	}
	return SSHInfo{SshPriv: sshPriv, SshPub: sshPub}, nil
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
	i := f.invoker(args).setCaptureOutput().setStrict()
	err := i.run(ctx)
	s := i.output.String()

	return processImageFromPBResult(pbPath, strings.TrimSpace(s), err)
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

// Log runs "ffx log <args>", optionally sending the output to "output"
func (f *FFXInstance) Log(ctx context.Context, output *io.Writer, args ...string) error {
	if f.target == "" {
		return fmt.Errorf("no target is set")
	}
	args = append([]string{"log"}, args...)
	i := f.invoker(args).setTarget(f.target).setMachineFormat(MachineRaw).setStrict()
	if output != nil {
		i.setStdout(*output)
	}
	return i.run(ctx)
}
