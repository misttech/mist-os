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
	"time"

	"golang.org/x/exp/slices"

	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/integration/testsharder"
	"go.fuchsia.dev/fuchsia/tools/lib/color"
	"go.fuchsia.dev/fuchsia/tools/lib/flagmisc"
	"go.fuchsia.dev/fuchsia/tools/lib/hostplatform"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

func usage() {
	fmt.Printf(`testsharder [flags]

Shards tests produced by a build.
`)
}

type testsharderFlags struct {
	buildDir                       string
	outputFile                     string
	tags                           flagmisc.StringsValue
	modifiersPath                  string
	targetTestCount                int
	targetDurationSecs             int
	perTestTimeoutSecs             int
	maxShardsPerEnvironment        int
	maxShardSize                   int
	affectedTestsPath              string
	affectedTestsMaxAttempts       int
	affectedTestsMultiplyThreshold int
	affectedOnly                   bool
	hermeticDeps                   bool
	imageDeps                      bool
	pave                           bool
	skipUnaffected                 bool
	perShardPackageRepos           bool
	cacheTestPackages              bool
	productBundleName              string
}

func parseFlags() testsharderFlags {
	var flags testsharderFlags
	flag.StringVar(&flags.buildDir, "build-dir", "", "path to the fuchsia build directory root (required)")
	flag.StringVar(&flags.outputFile, "output-file", "", "path to a file which will contain the shards as JSON, default is stdout")
	flag.Var(&flags.tags, "tag", "environment tags on which to filter; only the tests that match all tags will be sharded")
	flag.StringVar(&flags.modifiersPath, "modifiers", "", "path to the json manifest containing tests to modify")
	flag.IntVar(&flags.targetDurationSecs, "target-duration-secs", 0, "approximate duration that each shard should run in")
	flag.IntVar(&flags.maxShardsPerEnvironment, "max-shards-per-env", 8, "maximum shards allowed per environment. If <= 0, no max will be set")
	flag.IntVar(&flags.maxShardSize, "max-shard-size", 0, "target max number of tests per shard. It will only have effect if used with target-duration-secs to further "+
		"limit the number of tests per shard if the calculated average tests per shard would exceed max-shard-size after sharding by duration. This is only a soft "+
		"maximum and is used to make the average shard size not exceed the max size, but ultimately the shards will be sharded by duration, so some shards may have "+
		"more than the max number of tests while others will have less. However, if max-shards-per-env is set, that will take precedence over max-shard-size, which "+
		"may result in all shards exceeding the max size in order to fit within the max number of shards per environment.")
	// TODO(https://fxbug.dev/42055729): Support different timeouts for different tests.
	flag.IntVar(&flags.perTestTimeoutSecs, "per-test-timeout-secs", 0, "per-test timeout, applied to all tests. If <= 0, no timeout will be set")
	flag.IntVar(&flags.targetTestCount, "target-test-count", 0, "target number of tests per shard. If <= 0, will be ignored. Otherwise, tests will be placed into more, smaller shards")
	flag.StringVar(&flags.affectedTestsPath, "affected-tests", "", "path to a file containing names of tests affected by the change being tested. One test name per line.")
	flag.IntVar(&flags.affectedTestsMaxAttempts, "affected-tests-max-attempts", 2, "maximum attempts for each affected test. Only applied to tests that are not multiplied")
	flag.IntVar(&flags.affectedTestsMultiplyThreshold, "affected-tests-multiply-threshold", 0, "if there are <= this many tests in -affected-tests, they may be multplied "+
		"(modified to run many times in a separate shard), but only be multiplied if allowed by certain constraints designed to minimize false rejections and bot demand.")
	flag.BoolVar(&flags.affectedOnly, "affected-only", false, "whether to create test shards for only the affected tests found in either the modifiers file or the affected-tests file.")
	flag.BoolVar(&flags.hermeticDeps, "hermetic-deps", false, "whether to add all the images and blobs used by the shard as dependencies")
	flag.BoolVar(&flags.imageDeps, "image-deps", false, "whether to add all the images used by the shard as dependencies")
	flag.BoolVar(&flags.pave, "pave", false, "whether the shards generated should pave or netboot fuchsia")
	flag.BoolVar(&flags.skipUnaffected, "skip-unaffected", false, "whether the shards should ignore hermetic, unaffected tests")
	flag.BoolVar(&flags.perShardPackageRepos, "per-shard-package-repos", false, "whether to construct a local package repo for each shard")
	flag.BoolVar(&flags.cacheTestPackages, "cache-test-packages", false, "whether the test packages should be cached on disk in the local package repo")
	flag.StringVar(&flags.productBundleName, "product-bundle-name", "", "name of product bundle to use")
	flag.Usage = usage

	flag.Parse()

	return flags
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

	if flags.buildDir == "" {
		return fmt.Errorf("must specify a Fuchsia build output directory")
	}

	m, err := build.NewModules(flags.buildDir)
	if err != nil {
		return err
	}
	return execute(ctx, flags, m)
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
	ProductBundles() []build.ProductBundle
}

// for testability
var getHostPlatform = func() (string, error) {
	return hostplatform.Name()
}

func execute(ctx context.Context, flags testsharderFlags, m buildModules) error {
	targetDuration := time.Duration(flags.targetDurationSecs) * time.Second
	if flags.targetTestCount > 0 && targetDuration > 0 {
		return fmt.Errorf("max-shard-size and target-duration-secs cannot both be set")
	}

	if flags.maxShardSize > 0 {
		if flags.targetTestCount > 0 {
			return fmt.Errorf("max-shard-size has no effect when used with target-test-count")
		}
		if targetDuration == 0 {
			// If no target duration is set, then max shard size will effectively just
			// become the target test count.
			flags.targetTestCount = flags.maxShardSize
		}
	}

	perTestTimeout := time.Duration(flags.perTestTimeoutSecs) * time.Second

	if err := testsharder.ValidateTests(m.TestSpecs(), m.Platforms()); err != nil {
		return err
	}

	opts := &testsharder.ShardOptions{
		Tags: flags.tags,
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
		affectedModifiers, err := testsharder.AffectedModifiers(m.TestSpecs(), affectedTestNames, flags.affectedTestsMaxAttempts, flags.affectedTestsMultiplyThreshold)
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

	shards, newTargetDuration := testsharder.WithTargetDuration(shards, targetDuration, flags.targetTestCount, flags.maxShardSize, flags.maxShardsPerEnvironment, testDurations)

	// Add the multiplied shards back into the list of shards to run.
	if newTargetDuration > targetDuration {
		targetDuration = newTargetDuration
	}
	multipliedShards = testsharder.SplitOutMultipliers(ctx, multipliedShards, testDurations, targetDuration, flags.targetTestCount, testsharder.MaxMultipliedRunsPerShard, testsharder.MultipliedShardPrefix)
	multipliedAffectedShards = testsharder.SplitOutMultipliers(ctx, multipliedAffectedShards, testDurations, targetDuration, flags.targetTestCount, testsharder.MaxMultipliedRunsPerShard, testsharder.AffectedShardPrefix)
	shards = append(multipliedAffectedShards, shards...)
	shards = append(shards, multipliedShards...)

	for _, s := range shards {
		if s.Env.Dimensions.DeviceType() == "" {
			continue
		}
		if err := testsharder.AddFFXDeps(s, flags.buildDir, m.Tools(), flags.pave); err != nil {
			return err
		}
		productBundle := flags.productBundleName
		if s.ProductBundle != "" {
			productBundle = s.ProductBundle
		}
		if productBundle == "" {
			return fmt.Errorf("-product-bundle-name must be provided")
		}
		pbPath := build.GetPbPathByName(m.ProductBundles(), productBundle)
		if pbPath == "" {
			return fmt.Errorf("product bundle %s is not included in the product_bundles.json manifest", productBundle)
		}
		platform, err := getHostPlatform()
		if err != nil {
			return err
		}
		ffxTool, err := m.Tools().LookupTool(platform, "ffx")
		if err != nil {
			return err
		}
		ffxPath := filepath.Join(flags.buildDir, ffxTool.Path)
		if err := testsharder.AddImageDeps(ctx, s, flags.buildDir, m.Images(), flags.pave, pbPath, ffxPath); err != nil {
			return err
		}
	}

	if flags.perShardPackageRepos || flags.hermeticDeps {
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
				if err := s.CreatePackageRepo(flags.buildDir, pkgRepos[0].Path, flags.cacheTestPackages || flags.hermeticDeps); err != nil {
					return err
				}
			}
		}
	}

	if err := testsharder.ExtractDeps(shards, flags.buildDir); err != nil {
		return err
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
