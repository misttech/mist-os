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

func getCommand(
	runner *subprocess.Runner,
	stdout, stderr io.Writer,
	args ...string,
) *exec.Cmd {
	return runner.Command(args, subprocess.RunOptions{
		Stdout: stdout,
		Stderr: stderr,
	})
}

// FFXInstance takes in a path to the ffx tool and runs ffx commands with the provided config.
type FFXInstance struct {
	ctx     context.Context
	ffxPath string

	runner     *subprocess.Runner
	stdout     io.Writer
	stderr     io.Writer
	target     string
	env        []string
	isolateDir string
}

// ConfigSettings contains settings to apply to the ffx configs at the specified config level.
type ConfigSettings struct {
	Level    string
	Settings map[string]any
}

// FFXWithTarget returns a copy of the provided ffx instance associated with
// the provided target. This copy should use the same ffx daemon but run
// commands with the new target.
func FFXWithTarget(ffx *FFXInstance, target string) *FFXInstance {
	return &FFXInstance{
		ctx:        ffx.ctx,
		ffxPath:    ffx.ffxPath,
		runner:     ffx.runner,
		stdout:     ffx.stdout,
		stderr:     ffx.stderr,
		target:     target,
		env:        ffx.env,
		isolateDir: ffx.isolateDir,
	}
}

// NewFFXInstance creates an isolated FFXInstance.
func NewFFXInstance(
	ctx context.Context,
	ffxPath string,
	dir string,
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

	env = append(os.Environ(), env...)
	env = append(env, fmt.Sprintf("%s=%s", FFXIsolateDirEnvKey, absOutputDir))
	absFFXPath, err := filepath.Abs(ffxPath)
	if err != nil {
		return nil, err
	}
	ffx := &FFXInstance{
		ctx:        ctx,
		ffxPath:    absFFXPath,
		runner:     &subprocess.Runner{Dir: dir, Env: env},
		stdout:     os.Stdout,
		stderr:     os.Stderr,
		target:     target,
		env:        env,
		isolateDir: absOutputDir,
	}
	ffxEnvFilepath := filepath.Join(ffx.isolateDir, ffxEnvFilename)
	globalConfigFilepath := filepath.Join(ffx.isolateDir, "global_config.json")
	userConfigFilepath := filepath.Join(ffx.isolateDir, "user_config.json")
	ffxEnvSettings := map[string]any{
		"user":   userConfigFilepath,
		"global": globalConfigFilepath,
	}
	// Set these fields in the global config for tests that don't use this library
	// and don't set their own isolated env config.
	globalConfigSettings := map[string]any{
		// This is a config "alias" for various other config values -- disabling
		// metrics, device discovery, device auto-connection, etc.
		"ffx.isolated": true,
	}
	configSettings := map[string]any{
		"log.dir":                      filepath.Join(absOutputDir, "ffx_logs"),
		"ffx.subtool-search-paths":     filepath.Dir(absFFXPath),
		"target.default":               target,
		"test.experimental_json_input": true,
	}
	if sshKey != "" {
		sshKey, err = filepath.Abs(sshKey)
		if err != nil {
			return nil, err
		}
		configSettings["ssh.priv"] = []string{sshKey}
	}
	for _, settings := range extraConfigSettings {
		if settings.Level == "global" {
			for key, val := range settings.Settings {
				globalConfigSettings[key] = val
			}
		} else {
			for key, val := range settings.Settings {
				configSettings[key] = val
			}
		}
	}
	ffxCmds := [][]string{}
	if deviceAddr := os.Getenv(botanistconstants.DeviceAddrEnvKey); deviceAddr != "" {
		globalConfigSettings["discovery.mdns.enabled"] = false
		ffxCmds = append(ffxCmds, []string{"target", "add", deviceAddr, "--nowait"})
	}
	if err := writeConfigFile(globalConfigFilepath, globalConfigSettings); err != nil {
		return nil, fmt.Errorf("failed to write ffx global config at %s: %w", globalConfigFilepath, err)
	}
	if err := writeConfigFile(userConfigFilepath, configSettings); err != nil {
		return nil, fmt.Errorf("failed to write ffx user config at %s: %w", userConfigFilepath, err)
	}
	if err := writeConfigFile(ffxEnvFilepath, ffxEnvSettings); err != nil {
		return nil, fmt.Errorf("failed to write ffx env file at %s: %w", ffxEnvFilepath, err)
	}
	for _, args := range ffxCmds {
		if err := ffx.Run(ctx, args...); err != nil {
			if stopErr := ffx.Stop(); stopErr != nil {
				logger.Debugf(ctx, "failed to stop daemon: %s", stopErr)
			}
			return nil, fmt.Errorf("failed to run ffx cmd: %v: %w", args, err)
		}
	}
	return ffx, nil
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

// Command returns an *exec.Cmd to run ffx with the provided args.
func (f *FFXInstance) Command(args ...string) *exec.Cmd {
	args = append([]string{f.ffxPath, "--isolate-dir", f.isolateDir}, args...)
	return getCommand(f.runner, f.stdout, f.stderr, args...)
}

// CommandWithTarget returns a Command to run with the associated target.
func (f *FFXInstance) CommandWithTarget(args ...string) *exec.Cmd {
	args = append([]string{"--target", f.target}, args...)
	return f.Command(args...)
}

// Run runs ffx with the associated config and provided args.
func (f *FFXInstance) Run(ctx context.Context, args ...string) error {
	cmd := f.Command(args...)
	if err := f.runner.RunCommand(ctx, cmd); err != nil {
		return fmt.Errorf("%s: %w", constants.CommandFailedMsg, err)
	}
	return nil
}

// RunWithTarget runs ffx with the associated target.
func (f *FFXInstance) RunWithTarget(ctx context.Context, args ...string) error {
	args = append([]string{"--target", f.target}, args...)
	return f.Run(ctx, args...)
}

// RunAndGetOutput runs ffx with the provided args and returns the stdout.
func (f *FFXInstance) RunAndGetOutput(ctx context.Context, args ...string) (string, error) {
	origStdout := f.stdout
	var output bytes.Buffer
	f.stdout = io.MultiWriter(&output, origStdout)
	defer func() {
		f.stdout = origStdout
	}()
	if err := f.Run(ctx, args...); err != nil {
		return "", err
	}
	return strings.TrimSpace(output.String()), nil
}

// WaitForDaemon tries a few times to check that the daemon is up
// and returns an error if it fails to respond.
func (f *FFXInstance) WaitForDaemon(ctx context.Context) error {
	// Discard the stderr since it'll return a string caught by
	// tefmocheck if the daemon isn't ready yet.
	origStderr := f.stderr
	f.stderr = io.Discard
	defer func() {
		f.stderr = origStderr
	}()
	return retry.Retry(ctx, retry.WithMaxAttempts(retry.NewConstantBackoff(time.Second), 3), func() error {
		return f.Run(ctx, "daemon", "echo")
	}, nil)
}

// Stop stops the daemon.
func (f *FFXInstance) Stop() error {
	// Use a new context for Stop() to give it time to complete.
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	// Wait up to 4000ms for daemon to shut down.
	return f.Run(ctx, "daemon", "stop", "-t", "4000")
}

// BootloaderBoot RAM boots the target.
func (f *FFXInstance) BootloaderBoot(ctx context.Context, serialNum, productBundle string) error {
	args := []string{
		"--target", serialNum,
		"--config", "{\"ffx\": {\"fastboot\": {\"inline_target\": true}}}",
		"target", "bootloader",
	}
	args = append(args, "--product-bundle", productBundle)
	args = append(args, "boot")
	return f.Run(ctx, args...)
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
	f.RunWithTarget(
		ctx,
		append(
			[]string{
				"test",
				"run",
				"--continue-on-timeout",
				"--test-file",
				testFile,
				"--output-directory",
				testOutputDir,
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
	output, err := f.RunAndGetOutput(ctx, "--config", "ffx_product_get_artifacts=true", "product", "get-artifacts", pbPath, "-r", "-g", artifactsGroup)
	if err != nil {
		return nil, err
	}

	return strings.Split(output, "\n"), nil
}

// GetImageFromPB returns an image from a product bundle.
func (f *FFXInstance) GetImageFromPB(ctx context.Context, pbPath string, slot string, imageType string, bootloader string) (*bootserver.Image, error) {
	args := []string{"--config", "ffx_product_get_image_path=true", "product", "get-image-path", pbPath, "-r"}
	if slot != "" && imageType != "" && bootloader == "" {
		args = append(args, "--slot", slot, "--image-type", imageType)
	} else if bootloader != "" && slot == "" && imageType == "" {
		args = append(args, "--bootloader", bootloader)
	} else {
		return nil, fmt.Errorf("either slot and image type should be provided or bootloader "+
			"should be provided, not both: slot: %s, imageType: %s, bootloader: %s", slot, imageType, bootloader)
	}
	relImagePath, err := f.RunAndGetOutput(ctx, args...)
	if err != nil {
		// An error is returned if the image cannot be found in the product bundle
		// which is ok.
		return nil, nil
	}

	imagePath := filepath.Join(pbPath, relImagePath)
	buildImg := build.Image{Name: relImagePath, Path: imagePath}

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
