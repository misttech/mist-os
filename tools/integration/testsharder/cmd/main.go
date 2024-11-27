// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"

	"golang.org/x/exp/slices"

	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/integration/testsharder"
	"go.fuchsia.dev/fuchsia/tools/integration/testsharder/proto"
	"go.fuchsia.dev/fuchsia/tools/lib/color"
	"go.fuchsia.dev/fuchsia/tools/lib/hostplatform"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"

	"google.golang.org/protobuf/encoding/prototext"
)

func usage() {
	fmt.Printf(`testsharder [flags]

Shards tests produced by a build.
`)
}

type testsharderFlags struct {
	buildDir                 string
	outputFile               string
	modifiersPath            string
	affectedTestsPath        string
	affectedTestsMaxAttempts int
	affectedOnly             bool
	skipUnaffected           bool
	testsharderParamsFile    string
}

func parseFlags() testsharderFlags {
	var flags testsharderFlags
	flag.StringVar(&flags.buildDir, "build-dir", "", "path to the fuchsia build directory root (required)")
	flag.StringVar(&flags.outputFile, "output-file", "", "path to a file which will contain the shards as JSON, default is stdout")
	flag.StringVar(&flags.modifiersPath, "modifiers", "", "path to the json manifest containing tests to modify")
	flag.StringVar(&flags.affectedTestsPath, "affected-tests", "", "path to a file containing names of tests affected by the change being tested. One test name per line.")
	flag.IntVar(&flags.affectedTestsMaxAttempts, "affected-tests-max-attempts", 2, "maximum attempts for each affected test. Only applied to tests that are not multiplied")
	flag.BoolVar(&flags.affectedOnly, "affected-only", false, "whether to create test shards for only the affected tests found in either the modifiers file or the affected-tests file.")
	flag.BoolVar(&flags.skipUnaffected, "skip-unaffected", false, "whether the shards should ignore hermetic, unaffected tests")
	flag.StringVar(&flags.testsharderParamsFile, "params-file", "", "path to the testsharder params file")

	flag.Usage = usage

	flag.Parse()

	return flags
}

// ReadParams deserializes a Params proto from a textproto file.
func ReadParams(path string) (*proto.Params, error) {
	bytes, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var message proto.Params
	if err := prototext.Unmarshal(bytes, &message); err != nil {
		return nil, err
	}
	if message.MaxShardsPerEnv == 0 {
		message.MaxShardsPerEnv = 8
	}
	return &message, nil
}

func main() {
	l := logger.NewLogger(logger.ErrorLevel, color.NewColor(color.ColorAuto), os.Stdout, os.Stderr, "")
	// testsharder is expected to complete quite quickly, so it's generally not
	// useful to include timestamps in logs. File names can be helpful though.
	l.SetFlags(logger.Lshortfile)
	ctx := logger.WithLogger(context.Background(), l)
	ctx, cancel := signal.NotifyContext(ctx, syscall.SIGTERM, syscall.SIGINT)
	defer cancel()

	if err := mainImpl(ctx); err != nil {
		logger.Fatalf(ctx, err.Error())
	}
}

func mainImpl(ctx context.Context) error {
	flags := parseFlags()
	if flags.testsharderParamsFile == "" {
		return fmt.Errorf("must provide a -params-file")

	}
	params, err := ReadParams(flags.testsharderParamsFile)
	if err != nil {
		return err
	}

	if flags.buildDir == "" {
		return fmt.Errorf("must specify a Fuchsia build output directory")
	}

	m, err := build.NewModules(flags.buildDir)
	if err != nil {
		return err
	}
	return execute(ctx, flags, params, m)
}

type buildModules interface {
	Args() build.Args
	Images() []build.Image
	Platforms() []build.DimensionSet
	TestSpecs() []build.TestSpec
	TestListLocation() []string
	TestDurations() []build.TestDuration
	Tools() build.Tools
	PackageRepositories() []build.PackageRepo
	PrebuiltVersions() ([]build.PrebuiltVersion, error)
	ProductBundles() []build.ProductBundle
}

// for testability
var getHostPlatform = func() (string, error) {
	return hostplatform.Name()
}

func execute(ctx context.Context, flags testsharderFlags, params *proto.Params, m buildModules) error {
	targetDuration := params.TargetDuration.AsDuration()
	if params.TargetTestCount > 0 && targetDuration > 0 {
		return fmt.Errorf("max-shard-size and target-duration-secs cannot both be set")
	}

	if params.MaxShardSize > 0 {
		if params.TargetTestCount > 0 {
			return fmt.Errorf("max-shard-size has no effect when used with target-test-count")
		}
		if targetDuration == 0 {
			// If no target duration is set, then max shard size will effectively just
			// become the target test count.
			params.TargetTestCount = params.MaxShardSize
		}
	}

	perTestTimeout := params.PerTestTimeout.AsDuration()

	if err := testsharder.ValidateTests(m.TestSpecs(), m.Platforms()); err != nil {
		return err
	}

	opts := &testsharder.ShardOptions{
		Tags: params.EnvironmentTags,
	}
	// Pass in the test-list to carry over tags to the shards.
	testListPath := filepath.Join(flags.buildDir, m.TestListLocation()[0])
	testListEntries, err := build.LoadTestList(testListPath)
	if err != nil {
		return err
	}

	// The "cpu" dimension for host and emulator tests has an effective default
	// value set by the recipes corresponding to the value of the "target_cpu"
	// GN arg. If test X sets "cpu" to its default value and test Y doesn't set
	// "cpu", the two tests should still be sharded together.
	//
	// TODO(olivernewman): It's hacky to hardcode the defaults here when they're
	// actually applied by recipes. Centralize the default values in the build
	// system.
	testSpecs := m.TestSpecs()
	var defaultCPU string
	if err := m.Args().Get("target_cpu", &defaultCPU); err != nil {
		return fmt.Errorf("failed to look up value of target_cpu arg: %w", err)
	}
	for ti := range testSpecs {
		for ei, env := range testSpecs[ti].Envs {
			dt := env.Dimensions.DeviceType()
			// Only applies to host and emulator tests.
			if dt == "" || strings.HasSuffix(dt, "EMU") {
				if _, ok := env.Dimensions["cpu"]; !ok {
					testSpecs[ti].Envs[ei].Dimensions["cpu"] = defaultCPU
				}
			}
		}
	}
	shards := testsharder.MakeShards(m.TestSpecs(), testListEntries, opts)

	if perTestTimeout > 0 {
		testsharder.ApplyTestTimeouts(shards, perTestTimeout)
	}

	testDurations := testsharder.NewTestDurationsMap(m.TestDurations())
	shards = testsharder.AddExpectedDurationTags(shards, testDurations)

	if flags.modifiersPath != "" {
		modifiers, err := testsharder.LoadTestModifiers(ctx, m.TestSpecs(), flags.modifiersPath)
		if err != nil {
			return err
		}
		// Apply user-defined modifiers.
		shards, err = testsharder.ApplyModifiers(shards, modifiers)
		if err != nil {
			return err
		}
	}

	// Remove the multiplied shards from the set of shards to analyze for
	// affected tests, as we want to run these shards regardless of whether
	// the associated tests are affected.
	multiplied := func(t testsharder.Test) bool {
		return t.RunAlgorithm == testsharder.StopOnFailure
	}
	multipliedShards, nonMultipliedShards := testsharder.PartitionShards(shards, multiplied, "")

	if flags.affectedTestsPath != "" {
		affectedTestBytes, err := os.ReadFile(flags.affectedTestsPath)
		if err != nil {
			return fmt.Errorf("failed to read affectedTestsPath (%s): %w", flags.affectedTestsPath, err)
		}
		affectedTestNames := strings.Split(strings.TrimSpace(string(affectedTestBytes)), "\n")
		if len(affectedTestNames) == 1 && affectedTestNames[0] == "" {
			// If the affected tests file is empty, strings.Split()
			// will return a list of one element with the empty string
			// in it.
			// If there are no affected tests, that means we weren't
			// able to determine which tests were affected so we should
			// run all tests.
			flags.skipUnaffected = false
		}
		affectedModifiers, err := testsharder.AffectedModifiers(m.TestSpecs(), affectedTestNames, flags.affectedTestsMaxAttempts, int(params.AffectedTestsMultiplyThreshold))
		if err != nil {
			return err
		}
		// Apply affected modifiers to both multiplied and non-multiplied shards
		// so that tests in all shards are correctly labeled as affected.
		multipliedShards, err = testsharder.ApplyModifiers(multipliedShards, affectedModifiers)
		if err != nil {
			return err
		}
		nonMultipliedShards, err = testsharder.ApplyModifiers(nonMultipliedShards, affectedModifiers)
		if err != nil {
			return err
		}
	} else {
		// If no affected-tests file was provided, we don't know which tests
		// were affected, so run all tests.
		flags.skipUnaffected = false
	}

	// Remove the multiplied affected shards from the set of shards to analyze for
	// affected tests, as we want to run these shards separately from the rest.
	multipliedAffected := func(t testsharder.Test) bool {
		return t.Affected && t.RunAlgorithm == testsharder.StopOnFailure
	}
	multipliedAffectedShards, nonMultipliedShards := testsharder.PartitionShards(nonMultipliedShards, multipliedAffected, "")

	var skippedShards []*testsharder.Shard
	if flags.affectedOnly && flags.skipUnaffected {
		affected := func(t testsharder.Test) bool {
			return t.Affected
		}
		affectedShards, unaffectedShards := testsharder.PartitionShards(nonMultipliedShards, affected, testsharder.AffectedShardPrefix)
		shards = affectedShards
		skippedShards, err = testsharder.MarkShardsSkipped(unaffectedShards)
		if err != nil {
			return err
		}
	} else {
		// Filter out the affected, hermetic shards from the non-multiplied shards.
		hermeticAndAffected := func(t testsharder.Test) bool {
			return t.Affected && t.Hermetic()
		}
		affectedHermeticShards, unaffectedOrNonhermeticShards := testsharder.PartitionShards(nonMultipliedShards, hermeticAndAffected, testsharder.AffectedShardPrefix)
		shards = affectedHermeticShards

		// Filter out unaffected hermetic shards from the remaining shards.
		hermetic := func(t testsharder.Test) bool {
			return t.Hermetic()
		}
		unaffectedHermeticShards, nonhermeticShards := testsharder.PartitionShards(unaffectedOrNonhermeticShards, hermetic, testsharder.HermeticShardPrefix)
		if flags.skipUnaffected {
			// Mark the unaffected, hermetic shards skipped, as we don't need to
			// run them.
			skippedShards, err = testsharder.MarkShardsSkipped(unaffectedHermeticShards)
			if err != nil {
				return err
			}
		} else {
			shards = append(shards, unaffectedHermeticShards...)
		}
		// The shards should include:
		// 1. Affected hermetic shards
		// 2. Unaffected hermetic shards (may be skipped)
		// 3. Nonhermetic shards
		shards = append(shards, nonhermeticShards...)
	}

	shards, newTargetDuration := testsharder.WithTargetDuration(shards, targetDuration, int(params.TargetTestCount), int(params.MaxShardSize), int(params.MaxShardsPerEnv), testDurations)

	// Add the multiplied shards back into the list of shards to run.
	if newTargetDuration > targetDuration {
		targetDuration = newTargetDuration
	}
	multipliedShards = testsharder.SplitOutMultipliers(ctx, multipliedShards, testDurations, targetDuration, int(params.TargetTestCount), testsharder.MaxMultipliedRunsPerShard, testsharder.MultipliedShardPrefix)
	multipliedAffectedShards = testsharder.SplitOutMultipliers(ctx, multipliedAffectedShards, testDurations, targetDuration, int(params.TargetTestCount), testsharder.MaxMultipliedRunsPerShard, testsharder.AffectedShardPrefix)
	shards = append(multipliedAffectedShards, shards...)
	shards = append(shards, multipliedShards...)

	platform, err := getHostPlatform()
	if err != nil {
		return err
	}
	ffxTool, err := m.Tools().LookupTool(platform, "ffx")
	if err != nil {
		return err
	}
	ffxPath := filepath.Join(flags.buildDir, ffxTool.Path)
	prebuiltVersions, err := m.PrebuiltVersions()
	if err != nil {
		return err
	}
	for _, s := range shards {
		// Pave = false means netboot = true. We set this after creating the shards
		// so that `netboot` doesn't get added to the shard names since this would
		// apply to all shards.
		if !params.Pave {
			s.Env.Netboot = true
		}
		if s.Env.Dimensions.DeviceType() == "" {
			continue
		}
		if err := testsharder.AddEmuVersion(s, prebuiltVersions); err != nil {
			return err
		}
		if err := testsharder.AddFFXDeps(s, flags.buildDir, m.Tools(), params.Pave); err != nil {
			return err
		}
		if s.ProductBundle == "" {
			s.ProductBundle = params.ProductBundleName
		}
		if s.ProductBundle == "" {
			return fmt.Errorf("-product-bundle-name must be provided")
		}
		pbPath := build.GetPbPathByName(m.ProductBundles(), s.ProductBundle)
		if pbPath == "" {
			return fmt.Errorf("product bundle %s is not included in the product_bundles.json manifest", s.ProductBundle)
		}
		if err := testsharder.AddImageDeps(ctx, s, flags.buildDir, m.Images(), params.Pave, pbPath, ffxPath); err != nil {
			return err
		}
	}

	if params.PerShardPackageRepos || params.HermeticDeps {
		pkgRepos := m.PackageRepositories()
		if len(pkgRepos) < 1 {
			return errors.New("build did not generate a package repository")
		}
		absPkgRepoPath := filepath.Join(flags.buildDir, pkgRepos[0].Path)
		if _, err := os.Stat(absPkgRepoPath); errors.Is(err, os.ErrNotExist) {
			logger.Warningf(ctx, "package repository %s does not exist, not creating per-shard package repos", absPkgRepoPath)
		} else if err != nil {
			return err
		} else {
			for _, s := range shards {
				if !s.ExpectsSSH {
					// A package repo is only needed for tests run over SSH and
					// thus unnecessary for shards running against product bundles
					// that don't support SSH.
					continue
				}
				if err := s.CreatePackageRepo(flags.buildDir, pkgRepos[0].Path, params.CacheTestPackages || params.HermeticDeps); err != nil {
					return err
				}
			}
		}
	}

	if err := testsharder.ExtractDeps(shards, flags.buildDir); err != nil {
		return err
	}

	// If the shard timeout is provided in the params file, use that instead
	// of the computed shard timeout.
	if params.ShardTimeout.AsDuration() > 0 {
		for _, s := range shards {
			s.TimeoutSecs = int(params.ShardTimeout.GetSeconds())
		}
	}

	// Add back the skipped shards so that we can process and upload results
	// downstream.
	shards = append(shards, skippedShards...)

	f := os.Stdout
	if flags.outputFile != "" {
		var err error
		f, err = os.Create(flags.outputFile)
		if err != nil {
			return fmt.Errorf("unable to create %s: %v", flags.outputFile, err)
		}
		defer f.Close()
	}

	slices.SortFunc(shards, func(a, b *testsharder.Shard) int {
		return strings.Compare(a.Name, b.Name)
	})

	encoder := json.NewEncoder(f)
	// Use 4-space indents so golden files are compatible with `fx format-code`.
	encoder.SetIndent("", "    ")
	if err := encoder.Encode(&shards); err != nil {
		return fmt.Errorf("failed to encode shards: %v", err)
	}

	// All shard names must be unique. Validate this *after* emitting the shards
	// so that it's easy to look at the output to see why dupes may have
	// occurred.
	if dupes := duplicatedShardNames(shards); len(dupes) > 0 {
		return fmt.Errorf("some shard names are repeated: %s", strings.Join(dupes, ", "))
	}
	return nil
}

func duplicatedShardNames(shards []*testsharder.Shard) []string {
	nameCounts := make(map[string]int)
	for _, s := range shards {
		nameCounts[s.Name]++
	}
	var dupes []string
	for name, count := range nameCounts {
		if count > 1 {
			dupes = append(dupes, name)
		}
	}
	return dupes
}
