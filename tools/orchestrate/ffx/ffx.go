// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Package ffx provides wrappers and convenience functions for using the ffx binaries.
package orchestrate

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	utils "go.fuchsia.dev/fuchsia/tools/orchestrate/utils"
)

type pathType struct {
	File string `json:"file"`
	URL  string `json:"url"`
}

type repoServerList struct {
	Result struct {
		Data []struct {
			Name                          string   `json:"name"`
			Address                       string   `json:"address"`
			RepoPath                      pathType `json:"repo_path"`
			RegistrationAliases           []string `json:"registration_aliases"`
			RegistrationStorageType       string   `json:"registration_storage_type"`
			RegistrationAliasConflictMode string   `json:"registration_alias_conflict_mode"`
			ServerMode                    string   `json:"server_mode"`
			PID                           int      `json:"pid"`
		} `json:"data"`
	} `json:"ok"`
}

// XDG_ENV_VARS are leaky environment variables to override. See ApplyEnv.
var XDG_ENV_VARS = [...]string{
	"HOME",
	"XDG_CACHE_HOME",
	"XDG_CONFIG_HOME",
	"XDG_DATA_HOME",
	"XDG_HOME",
	"XDG_STATE_HOME",
}

// Ffx defines settings for ffx commands.
type Ffx struct {
	// Dir is the working directory for ffx, and where it writes files.
	Dir         string
	bin         string
	sslCertPath string
}

// Option for creating Ffx.
type Option struct {
	// IsolateDir is the directory where all ffx state is stored. This can be
	// used to allow multiple instances of ffx to share the same configuration.
	// If the directory contents are empty, a default config will be generated.
	// If not set, a temporary directory will be created for this instance of
	// Ffx to use.
	IsolateDir string

	// ExePath is the path to the ffx cli tool.
	ExePath string

	// SSLCertPath is the path to the SSL certificates that ffx should use. If
	// not set, the default certificates will be used from the runfiles.
	// If the runfiles are not available, then the system paths will be searched
	// for appropriate certificates.
	SSLCertPath string

	// LogDir is the directory where ffx logs are stored.
	// If not set, it defaults to a sub-directory of the IsolateDir.
	//
	// Note: This option is only used to write the default ffx config when
	// initializing an empty isolate directory. Otherwise, the existing config in
	// IsolateDir is used.
	LogDir string

	// PrivateSSH is the list of the pregenerated private ssh key files that ffx
	// should use to connect to targets.
	//
	// Note: This option is only used to write the default ffx config when
	// initializing an empty isolate directory. Otherwise, the existing config in
	// IsolateDir is used.
	PrivateSSH []string

	// PublicSSH is the list of the pregenerated public ssh key files and
	// authorized_keys files that ffx should install when flashing targets or
	// starting up emulator instances.
	//
	// Note: This option is only used to write the default ffx config when
	// initializing an empty isolate directory. Otherwise, the existing config in
	// IsolateDir is used.
	PublicSSH []string

	// EnableCSO enables circuit-switched-overnet in the ffx daemon.
	// If set to false, the ffx default value is used.
	//
	// Note: This option is only used to write the default ffx config when
	// initializing an empty isolate directory. Otherwise, the existing config in
	// IsolateDir is used.
	EnableCSO bool
}

// New sets up a config and filepaths for local or Forge use.
func New(opt *Option) (*Ffx, error) {
	f := &Ffx{
		bin:         opt.ExePath,
		sslCertPath: opt.SSLCertPath,
		Dir:         opt.IsolateDir,
	}
	var err error
	if f.Dir == "" {
		if f.Dir, err = os.MkdirTemp("", "ffx"); err != nil {
			return nil, fmt.Errorf("create directory for ffx: %w", err)
		}
	}

	if f.bin == "" {
		return nil, errors.New("must provide path to ffx executable")
	}

	// Setup the default config if it has not been initialized yet.
	configPath := filepath.Join(f.Dir, ".ffx_user_config.json")
	if _, err := os.Stat(configPath); os.IsNotExist(err) {
		if err := f.setupDefaultConfig(configPath, *opt); err != nil {
			return nil, err
		}
	} else if err != nil {
		return nil, fmt.Errorf("unable to stat config file: %w", err)
	}
	return f, nil
}

func (f *Ffx) setupDefaultConfig(configPath string, opt Option) error {
	if opt.LogDir == "" {
		opt.LogDir = filepath.Join(f.Dir, "log")
	}
	if err := os.MkdirAll(opt.LogDir, 0755); err != nil {
		return err
	}

	socketPath := filepath.Join(f.Dir, "ascendd")
	err := writeConfigFile(configPath, opt, socketPath)
	if err != nil {
		return err
	}

	// Write the config to the isolation dir so that we don't need to pass it with every command.
	if out, err := f.RunCmdSync("config", "env", "set", configPath); err != nil {
		return fmt.Errorf("saving ffx config path: %s, %w", out, err)
	}
	return nil
}

// Cmd returns a generic exec.Cmd configured to execute ffx.
func (f *Ffx) Cmd(args ...string) (*exec.Cmd, error) {
	cmd := exec.Command(f.bin, args...)
	env, err := f.ApplyEnv(cmd.Environ())
	if err != nil {
		return nil, fmt.Errorf("Applying ffx env: %v", err)
	}
	cmd.Env = env
	return cmd, nil
}

// ApplyEnv adds the environment variables needed for safe execution of ffx.
func (f *Ffx) ApplyEnv(env []string) ([]string, error) {
	wd, err := os.Getwd()
	if err != nil {
		return nil, fmt.Errorf("os.Getwd: %v", err)
	}
	env = append(env, "FFX_ISOLATE_DIR="+f.Dir, "FUCHSIA_ANALYTICS_DISABLED=1")
	if f.sslCertPath != "" {
		env = append(env, "SSL_CERT_FILE="+f.sslCertPath)
	}
	// Override HOME and other HOME-related environment variables, since ffx and
	// tests shouldn't assume anything about those.
	// This prevents ffx from creating and using default ssh keys from the real
	// home directory.
	for _, xdg_env_var := range XDG_ENV_VARS {
		env = append(env, xdg_env_var+"="+f.Dir)
	}
	sshDir := filepath.Join(wd, "openssh-portable", "bin")
	// For non-daemon commands, the path to the ssh binary is required. Previously this was only
	// needed for execution of `ffx daemon start`.
	env = utils.AppendPath(env, sshDir)
	return env, nil
}

// RunCmdSync starts a command and waits for the command to complete.
func (f *Ffx) RunCmdSync(args ...string) (string, error) {
	cmd, err := f.Cmd(args...)
	if err != nil {
		return "", fmt.Errorf("Creating ffx command from %+v: %v", args, err)
	}
	log.Printf("Running command and streaming output: %+v", cmd.Args)

	// Pipe stderr to stdout, and then tee to a string builder.
	var output strings.Builder
	outputWriter := io.MultiWriter(&output, os.Stdout)
	cmd.Stdout = outputWriter
	cmd.Stderr = outputWriter

	if err := cmd.Run(); err != nil {
		return "", fmt.Errorf("cmd.Run: %w", err)
	}

	return output.String(), nil
}

// RunCmdAsync starts a command but does NOT wait for the command to complete.
func (f *Ffx) RunCmdAsync(args ...string) (*exec.Cmd, error) {
	cmd, err := f.Cmd(args...)
	if err != nil {
		return nil, fmt.Errorf("Creating ffx command from %+v: %v", args, err)
	}
	log.Printf("Running background command: %+v", cmd.Args)
	if err := cmd.Start(); err != nil {
		return cmd, fmt.Errorf("start command %s: %w", args, err)
	}
	return cmd, nil
}

// ConfigGet reads from the ffx config and writes to result as structured data.
func (f *Ffx) ConfigGet(field string, result any) error {
	out, err := f.RunCmdSync("config", "get", field)
	if err != nil {
		return fmt.Errorf("ffx config failed for %q: %w", field, err)
	}
	if err := json.Unmarshal([]byte(out), result); err != nil {
		return fmt.Errorf("unable to unmarshal config output: %w", err)
	}
	return nil
}

// Close removes all files from the ffx directory.
func (f *Ffx) Close() error {
	return os.RemoveAll(f.Dir)
}

// GetDefaultTarget fetches the default ffx target.
func (f *Ffx) GetDefaultTarget() (string, error) {
	defaultName, err := f.RunCmdSync("target", "default", "get")
	if err != nil {
		return "", fmt.Errorf("run \"target default get\" command. %s. %w", defaultName, err)
	}
	// An extra '\n' is added at the end of defaultName.
	return strings.TrimSpace(defaultName), nil
}

// WaitForDaemon tries a few times to check that the daemon is up
// and returns an error if it fails to respond.
func (f *Ffx) WaitForDaemon(ctx context.Context) error {
	return utils.RunWithRetries(context.Background(), 500*time.Millisecond, 3, func() error {
		_, err := f.RunCmdSync("daemon", "echo")
		return err
	})
}

// Flash uses "ffx target flash" to flash a product bundle into a device.
// pubKeyPath is optional and ignored if empty.
func (f *Ffx) Flash(fastbootSerial, productDir, pubKeyPath string) error {
	ffxArgs := []string{
		"--target", fastbootSerial,
		"--config", "{\"ffx\": {\"fastboot\": {\"inline_target\": true}}}",
		"target", "flash",
		"--product-bundle", productDir}
	if pubKeyPath != "" {
		ffxArgs = append(ffxArgs, "--authorized-keys", pubKeyPath)
	}
	_, err := f.RunCmdSync(ffxArgs...)
	return err
}

func writeConfigFile(configPath string, opt Option, socketPath string) error {
	overnet := map[string]string{"socket": socketPath}
	if opt.EnableCSO {
		overnet["cso"] = "enabled"
	}
	ssh := map[string][]string{}
	if len(opt.PrivateSSH) > 0 {
		ssh["priv"] = opt.PrivateSSH
	}
	if len(opt.PublicSSH) > 0 {
		ssh["pub"] = opt.PublicSSH
	}
	data := map[string]any{
		"overnet": overnet,
		"proxy": map[string]int{
			"timeout_secs": 60,
		},
		"ssh": ssh,
		"log": map[string]any{
			"dir":     []string{opt.LogDir},
			"enabled": []bool{true},
			"level":   "Debug",
		},
		"test": map[string]any{
			"suite_start_timeout_seconds": 600,
		},
	}
	j, err := json.Marshal(data)
	if err != nil {
		return fmt.Errorf("marshal ffx config: %w", err)
	}
	if err := os.WriteFile(configPath, j, 0o600); err != nil {
		return fmt.Errorf("writing ffx config to file: %w", err)
	}
	return nil
}

// isRunning returns true if the package server is currently running and responds to HTTP requests.
func (f *Ffx) IsPackageServerRunning(repoName string) (bool, error) {

	args := []string{"--machine", "json", "repository", "server", "list"}
	out, err := f.RunCmdSync(args...)
	if err != nil {
		return false, fmt.Errorf("ffx repository server list: output: %s, error: %w", out, err)
	}
	var repoList repoServerList
	if err := json.Unmarshal([]byte(out), &repoList); err != nil {
		return false, err
	}
	repoNamePrefix := fmt.Sprintf("%s.", repoName)
	for _, status := range repoList.Result.Data {
		// product bundle based repo servers use the repoName as a prefix.
		if status.Name == repoName || strings.HasPrefix(status.Name, repoNamePrefix) {
			return true, nil
		}
	}
	// We don't need to differentiate between a stopped package server, no server found, etc.
	return false, nil
}
