// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package testsharder

import (
	"fmt"
	"math/rand"
	"path/filepath"
	"slices"
	"strconv"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/integration/testsharder/proto"
	"go.fuchsia.dev/fuchsia/tools/lib/jsonutil"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

const (
	// The log level to use for botanist invocations in test tasks. Can be one of
	// "fatal", "error", "warning", "info", "debug", or "trace", where "trace" is
	// the most verbose, and fatal is the least.
	botanistLogLevel = logger.DebugLevel
)

func GetBotDimensions(shard *Shard, params *proto.Params) {
	dimensions := map[string]string{"pool": params.Pool}
	isEmuType := shard.Env.TargetsEmulator()
	testBotCpu := shard.HostCPU()

	isLinux := shard.Env.Dimensions.OS() == "" || strings.ToLower(shard.Env.Dimensions.OS()) == "linux"
	isGCEType := shard.Env.Dimensions.DeviceType() == "GCE"

	if isEmuType {
		dimensions["os"] = "Debian"
		if !params.UseTcg {
			dimensions["kvm"] = "1"
		}
		dimensions["cpu"] = testBotCpu
	} else if isGCEType {
		// Have any GCE shards target the GCE executors, which are e2-2
		// machines running Linux.
		dimensions["os"] = "Linux"
		dimensions["cores"] = "2"
		dimensions["gce"] = "1"
	} else {
		// No device -> no serial.
		if !shard.ExpectsSSH && shard.Env.Dimensions.DeviceType() != "" {
			dimensions["serial"] = "1"
		}
		for k, v := range shard.Env.Dimensions {
			dimensions[k] = v
		}
	}
	// Ensure we use GCE VMs whenever possible.
	if isLinux && !isGCEType && testBotCpu == "x64" && !params.UseTcg {
		dimensions["kvm"] = "1"
	}
	if (isEmuType || shard.Env.Dimensions.DeviceType() == "") && testBotCpu == "x64" && isLinux {
		dimensions["gce"] = "1"
		dimensions["cores"] = "8"
	}
	shard.BotDimensions = dimensions
}

func ConstructTestsJSON(shard *Shard, buildDir string) error {
	relTestManifest := shard.Name + "_tests.json"
	testManifest := filepath.Join(buildDir, relTestManifest)
	if err := jsonutil.WriteToFile(testManifest, shard.Tests); err != nil {
		return err
	}
	shard.TestsJSON = relTestManifest
	shard.AddDeps([]string{relTestManifest})
	return nil
}

// for testability
var randInt = func(n int) int {
	return rand.Intn(n)
}

func GetEnabledExperiments(experiments []string) ([]string, error) {
	enabledExperiments := []string{}
	for _, exp := range experiments {
		colonIndex := strings.LastIndex(exp, ":")
		if colonIndex > 0 {
			percentage, err := strconv.Atoi(exp[colonIndex+1:])
			if err == nil {
				if percentage < 0 || percentage > 100 {
					return nil, fmt.Errorf("invalid experiment percentage %d, must be between 0 and 100", percentage)
				}
				exp = exp[:colonIndex]
				r := randInt(100)
				if r >= percentage {
					continue
				}
			}
		}
		enabledExperiments = append(enabledExperiments, exp)
	}
	return enabledExperiments, nil
}

func registerTool(shard *Shard, tools build.Tools, toolName string) (string, error) {
	platform, err := testBotPlatform(shard.HostCPU())
	if err != nil {
		return "", err
	}
	tool, err := tools.LookupTool(platform, toolName)
	if err != nil {
		return "", err
	}
	shard.AddDeps([]string{tool.Path})
	return tool.Path, err
}

func ConstructBaseCommand(shard *Shard, checkoutRoot, buildDir string, tools build.Tools, params *proto.Params, variants, experiments []string) error {
	// Some artifacts are within the checkout root directory but not in the
	// build directory. Thus we need to map the task input tree root to the
	// checkout root directory instead. However, since the paths in the test
	// manifest are relative to the build directory, we use the relative
	// build directory as the relative cwd of the swarming task.
	relativeCWD, err := filepath.Rel(checkoutRoot, buildDir)
	if err != nil {
		return err
	}
	shard.RelativeCWD = relativeCWD

	botanist, err := registerTool(shard, tools, "botanist")
	if err != nil {
		return err
	}
	level := botanistLogLevel
	cmd := []string{
		"./" + botanist,
		"-level",
		level.String(),
		"run",
	}

	if shard.Env.Dimensions.OS() == "Linux" || shard.Env.Dimensions.OS() == "Mac" {
		cmd = append(cmd, "-skip-setup")
		shard.BaseCommand = cmd
		return nil
	}

	if slices.Contains(variants, "coverage-rust") {
		llvmProfdata, err := registerTool(shard, tools, "llvm-profdata")
		if err != nil {
			return err
		}
		cmd = append(cmd, "-llvm-profdata", fmt.Sprintf("%s=rust", llvmProfdata))
	} else if slices.Contains(variants, "coverage") {
		llvmProfdata, err := registerTool(shard, tools, "llvm-profdata")
		if err != nil {
			return err
		}
		cmd = append(cmd, "-llvm-profdata", fmt.Sprintf("%s=clang", llvmProfdata))
	}
	cmd = append(cmd,
		// TODO(https://fxbug.dev/346868967): Remove when we've migrated off of paving.
		"-images",
		imagesManifest,
		"-timeout",
		fmt.Sprintf("%ds", shard.TimeoutSecs),
	)

	ffx, err := registerTool(shard, tools, "ffx")
	if err != nil {
		return err
	}
	cmd = append(cmd, "-ffx", "./"+ffx)
	for _, exp := range experiments {
		cmd = append(cmd, "-experiment", exp)
	}

	if shard.ProductBundle == "" {
		return fmt.Errorf("missing product bundle name")
	}

	cmd = append(cmd,
		"-product-bundles",
		productBundlesManifest,
		"-product-bundle-name",
		shard.ProductBundle,
	)
	if shard.IsBootTest {
		cmd = append(cmd, "-boot-test")
	}
	if shard.BootupTimeoutSecs > 0 {
		cmd = append(cmd, "-bootup-timeout", fmt.Sprintf("%ds", shard.BootupTimeoutSecs))
	}
	if shard.PkgRepo != "" {
		cmd = append(cmd,
			"-local-repo",
			shard.PkgRepo,
		)
	}

	if shard.ExpectsSSH {
		cmd = append(cmd, "-expects-ssh")
	} else {
		cmd = append(cmd, "-use-serial")
	}

	if shard.Env.Netboot {
		cmd = append(cmd, "-netboot")
	}

	for _, arg := range params.ZirconArgs {
		cmd = append(cmd, "-zircon-args", arg)
	}
	if shard.Env.TargetsEmulator() && params.UseTcg {
		// Used by botanist to scale the test timeout since tests can
		// run much slower on QEMU bots running with TCG.
		cmd = append(cmd, "-test-timeout-scale-factor", "2")
	}

	shard.BaseCommand = cmd
	return nil
}
