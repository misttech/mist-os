// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"syscall"

	flag "github.com/spf13/pflag"

	"go.fuchsia.dev/fuchsia/tools/integration/fint"
	fintpb "go.fuchsia.dev/fuchsia/tools/integration/fint/proto"
	"go.fuchsia.dev/fuchsia/tools/lib/color"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/lib/osmisc"
	"go.fuchsia.dev/fuchsia/tools/lib/subprocess"
)

const (
	// Optional env var set by the user, pointing to the directory in which
	// ccache artifacts should be cached between builds.
	ccacheDirEnvVar = "CCACHE_DIR"

	// fx ensures that this env var is set.
	checkoutDirEnvVar = "FUCHSIA_DIR"

	// Populated when fx's top-level `--dir` flag is set. Guaranteed to be absolute.
	buildDirEnvVar = "_FX_BUILD_DIR"

	// We'll fall back to using this build dir if neither `fx --dir` nor `fx set
	// --auto-dir` is specified.
	defaultBuildDir = "out/default"
)

type subprocessRunner interface {
	Run(ctx context.Context, cmd []string, options subprocess.RunOptions) error
}

// fxRunner is a utility for running fx commands as subprocesses.
type fxRunner struct {
	sr          subprocessRunner
	checkoutDir string
}

func (r *fxRunner) constructCommand(command string, args []string) []string {
	fxPath := filepath.Join(r.checkoutDir, "scripts", "fx-reentry")
	cmd := []string{fxPath, command}
	return append(cmd, args...)
}

// run runs the given fx command with optional args.
func (r *fxRunner) run(ctx context.Context, command string, args ...string) error {
	return r.sr.Run(ctx, r.constructCommand(command, args), subprocess.RunOptions{
		// Subcommands may run interactive logins, so give them access to stdin by default.
		Stdin: os.Stdin,
	})
}

// runWithNoStdio is the same as run, but discards any stdout and stderr and
// doesn't forward stdin to the subprocess.
func (r *fxRunner) runWithNoStdio(ctx context.Context, command string, args ...string) error {
	return r.sr.Run(ctx, r.constructCommand(command, args), subprocess.RunOptions{
		Stdout: io.Discard, Stderr: io.Discard,
	})
}

func main() {
	l := logger.NewLogger(logger.ErrorLevel, color.NewColor(color.ColorAuto), os.Stdout, os.Stderr, "")
	// Don't include timestamps or other metadata in logs, since this tool is
	// only intended to be run on developer workstations.
	l.SetFlags(0)
	ctx := logger.WithLogger(context.Background(), l)
	ctx, cancel := signal.NotifyContext(ctx, syscall.SIGTERM, syscall.SIGINT)
	defer cancel()

	if err := mainImpl(ctx); err != nil {
		if ctx.Err() == nil {
			logger.Errorf(ctx, err.Error())
		}
		os.Exit(1)
	}
}

func mainImpl(ctx context.Context) error {
	args, err := parseArgsAndEnv(os.Args[1:], allEnvVars())
	if err != nil {
		return err
	}

	if args.verbose {
		if l := logger.LoggerFromContext(ctx); l != nil {
			l.LoggerLevel = logger.DebugLevel
		}
	}

	fx := fxRunner{
		sr:          &subprocess.Runner{},
		checkoutDir: args.checkoutDir,
	}

	var staticSpec *fintpb.Static
	if args.fintParamsPath == "" {
		canUseRbe, err := canAccessRbe(args.checkoutDir)
		if err != nil {
			fmt.Printf("Unable to determine RBE access, assuming False.")
			canUseRbe = false
		}
		staticSpec, err = constructStaticSpec(ctx, fx, args.checkoutDir, args, canUseRbe)
		if err != nil {
			return err
		}
	} else {
		path := args.fintParamsPath
		if !filepath.IsAbs(path) {
			path = filepath.Join(args.checkoutDir, path)
		}
		staticSpec, err = fint.ReadStatic(path)
		if err != nil {
			return err
		}
	}

	contextSpec := &fintpb.Context{
		CheckoutDir: args.checkoutDir,
		BuildDir:    filepath.Join(args.checkoutDir, args.buildDir),
	}

	_, err = fint.Set(ctx, staticSpec, contextSpec, args.skipLocalArgs)
	if err != nil {
		return err
	}

	// Set the build dir used by subsequent fx commands.
	buildDir := contextSpec.BuildDir
	if relBuildDir, err := filepath.Rel(contextSpec.CheckoutDir, contextSpec.BuildDir); err == nil {
		buildDir = relBuildDir
	}
	if err := fx.run(ctx, "use", buildDir); err != nil {
		return fmt.Errorf("failed to set build directory: %w", err)
	}

	if staticSpec.UseGoma && !args.noEnsureGoma {
		// Make sure Goma is set up.
		if err := fx.run(ctx, "goma"); err != nil {
			return err
		}
	}

	return nil
}

type setArgs struct {
	verbose        bool
	fintParamsPath string

	checkoutDir   string
	noEnsureGoma  bool
	buildDir      string
	skipLocalArgs bool

	// Flags passed to GN.
	board     string
	product   string
	useGoma   bool
	noGoma    bool
	gomaDir   string
	useCcache bool
	noCcache  bool
	ccacheDir string

	enableRbe     bool // deprecated, renamed
	enableRustRbe bool

	enableLinkRbe  bool
	enableBazelRbe bool

	enableCxxRbe  bool
	disableCxxRbe bool

	includeClippy bool

	isRelease        bool
	netboot          bool
	cargoTOMLGen     bool
	jsonIDEScripts   []string
	universePackages []string
	basePackages     []string
	cachePackages    []string
	hostLabels       []string
	testLabels       []string
	variants         []string
	fuzzSanitizers   []string
	ideFiles         []string
	gnArgs           []string
}

func parseArgsAndEnv(args []string, env map[string]string) (*setArgs, error) {
	cmd := &setArgs{}

	cmd.checkoutDir = env[checkoutDirEnvVar]
	if cmd.checkoutDir == "" {
		return nil, fmt.Errorf("%s env var must be set", checkoutDirEnvVar)
	}
	cmd.ccacheDir = env[ccacheDirEnvVar] // Not required.

	cmd.buildDir = env[buildDirEnvVar] // Not required.

	flagSet := flag.NewFlagSet("fx set", flag.ExitOnError)
	// TODO(olivernewman): Decide whether to have this tool print usage or
	// to let //tools/devshell/set handle usage.
	flagSet.Usage = func() {}
	// We log a final error to stderr, so no need to have pflag print
	// intermediate errors.
	flagSet.SetOutput(io.Discard)

	flagSet.BoolVar(&cmd.skipLocalArgs, "skip-local-args", false, "")

	var autoDir bool

	// Help strings don't matter because `fx set -h` uses the help text from
	// //tools/devshell/set, which should be kept up to date with these flags.
	flagSet.BoolVar(&cmd.verbose, "verbose", false, "")
	flagSet.BoolVar(&autoDir, "auto-dir", false, "")
	flagSet.StringVar(&cmd.fintParamsPath, "fint-params-path", "", "")
	flagSet.BoolVar(&cmd.useCcache, "ccache", false, "")
	flagSet.BoolVar(&cmd.noCcache, "no-ccache", false, "")
	flagSet.BoolVar(&cmd.useGoma, "goma", false, "")
	flagSet.BoolVar(&cmd.noGoma, "no-goma", false, "")
	flagSet.BoolVar(&cmd.noEnsureGoma, "no-ensure-goma", false, "")
	flagSet.BoolVar(&cmd.includeClippy, "include-clippy", true, "")
	// TODO(haowei): Remove --goma-dir once no other scripts use it.
	// We don't bother validating its value because the value isn't used
	// anywhere.
	flagSet.StringVar(&cmd.gomaDir, "goma-dir", "", "")

	flagSet.MarkDeprecated("rbe", "Use tool-specific controls like --rust-rbe instead")
	flagSet.BoolVar(&cmd.enableRustRbe, "rust-rbe", false, "")
	flagSet.BoolVar(&cmd.enableCxxRbe, "cxx-rbe", false, "")
	flagSet.BoolVar(&cmd.disableCxxRbe, "no-cxx-rbe", false, "")
	flagSet.BoolVar(&cmd.enableLinkRbe, "link-rbe", false, "")
	flagSet.BoolVar(&cmd.enableBazelRbe, "bazel-rbe", false, "")

	flagSet.BoolVar(&cmd.isRelease, "release", false, "")
	flagSet.BoolVar(&cmd.netboot, "netboot", false, "")
	flagSet.BoolVar(&cmd.cargoTOMLGen, "cargo-toml-gen", false, "")
	flagSet.StringSliceVar(&cmd.jsonIDEScripts, "json-ide-script", []string{}, "")
	flagSet.StringSliceVar(&cmd.universePackages, "with", []string{}, "")
	flagSet.StringSliceVar(&cmd.basePackages, "with-base", []string{}, "")
	flagSet.StringSliceVar(&cmd.cachePackages, "with-cache", []string{}, "")
	flagSet.StringSliceVar(&cmd.hostLabels, "with-host", []string{}, "")
	flagSet.StringSliceVar(&cmd.testLabels, "with-test", []string{}, "")
	flagSet.StringSliceVar(&cmd.variants, "variant", []string{}, "")
	flagSet.StringSliceVar(&cmd.fuzzSanitizers, "fuzz-with", []string{}, "")
	flagSet.StringSliceVar(&cmd.ideFiles, "ide", []string{}, "")
	// Unlike StringSliceVar, StringArrayVar doesn't split flag values at
	// commas. Commas are syntactically significant in GN, so they should be
	// preserved rather than interpreting them as value separators.
	flagSet.StringArrayVar(&cmd.gnArgs, "args", []string{}, "")

	if err := flagSet.Parse(args); err != nil {
		return nil, err
	}

	if cmd.buildDir == "" {
		cmd.buildDir = defaultBuildDir
	} else if autoDir {
		return nil, fmt.Errorf("'fx --dir' and 'fx set --auto-dir' are mutually exclusive")
	}

	// If a fint params file was specified then no other arguments are required,
	// so no need to validate them.
	if cmd.fintParamsPath != "" {
		if autoDir {
			return nil, fmt.Errorf("--auto-dir is not supported with --fint-params-path")
		}
		return cmd, nil
	}

	if cmd.useCcache && cmd.useGoma {
		return nil, fmt.Errorf("--goma and --ccache are mutually exclusive")
	}
	if cmd.useCcache && cmd.noCcache {
		return nil, fmt.Errorf("--ccache and --no-ccache are mutually exclusive")
	}

	if cmd.useGoma && cmd.noGoma {
		return nil, fmt.Errorf("--goma and --no-goma are mutually exclusive")
	} else if cmd.noGoma && cmd.gomaDir != "" {
		return nil, fmt.Errorf("--goma-dir and --no-goma are mutually exclusive")
	}
	if cmd.enableRbe {
		return nil, fmt.Errorf("--rbe (deprecated) was renamed to --rust-rbe.")
	}
	if cmd.enableCxxRbe && cmd.useCcache {
		return nil, fmt.Errorf("--cxx-rbe and --use-ccache are mutually exclusive")
	}
	if cmd.enableCxxRbe && cmd.useGoma {
		return nil, fmt.Errorf("--cxx-rbe and --use-goma are mutually exclusive")
	}
	if cmd.enableCxxRbe && cmd.disableCxxRbe {
		return nil, fmt.Errorf("--cxx-rbe and --no-cxx-rbe are mutually exclusive")
	}

	if flagSet.NArg() == 0 {
		return nil, fmt.Errorf("missing a PRODUCT.BOARD argument")
	} else if flagSet.NArg() > 1 {
		return nil, fmt.Errorf("only one positional PRODUCT.BOARD argument allowed")
	}

	productDotBoard := flagSet.Arg(0)
	productAndBoard := strings.Split(productDotBoard, ".")
	if len(productAndBoard) != 2 {
		return nil, fmt.Errorf("unable to parse PRODUCT.BOARD: %q", productDotBoard)
	}
	cmd.product, cmd.board = productAndBoard[0], productAndBoard[1]

	if autoDir {
		for _, variant := range cmd.variants {
			if strings.Contains(variant, "/") {
				return nil, fmt.Errorf(
					"--auto-dir only works with simple catch-all --variant switches; choose your " +
						"own directory name with fx --dir for a complex configuration")
			}
		}
		nameComponents := []string{productDotBoard}
		nameComponents = append(nameComponents, cmd.variants...)
		if cmd.isRelease {
			nameComponents = append(nameComponents, "release")
		}
		cmd.buildDir = filepath.Join("out", strings.Join(nameComponents, "-"))
	}

	return cmd, nil
}

func rbeIsSupported() bool {
	return (runtime.GOOS == "linux") && (runtime.GOARCH == "amd64")
}

func constructStaticSpec(ctx context.Context, fx fxRunner, checkoutDir string, args *setArgs, canUseRbe bool) (*fintpb.Static, error) {
	productPath, err := findGNIFile(checkoutDir, "products", args.product)
	if err != nil {
		productPath, err = findGNIFile(checkoutDir, filepath.Join("products", "tests"), args.product)
	}
	if err != nil {
		return nil, fmt.Errorf("no such product %q", args.product)
	}
	boardPath, err := findGNIFile(checkoutDir, "boards", args.board)
	if err != nil {
		return nil, fmt.Errorf("no such board: %q", args.board)
	}

	optimize := fintpb.Static_DEBUG
	if args.isRelease {
		optimize = fintpb.Static_RELEASE
	}

	variants := args.variants
	for _, sanitizer := range args.fuzzSanitizers {
		variants = append(variants, fuzzerVariants(sanitizer)...)
	}

	// Check for RBE eligibility.
	rbeSupported := rbeIsSupported()
	requestedAnyRbe := args.enableCxxRbe || args.enableRustRbe || args.enableLinkRbe || args.enableBazelRbe
	if requestedAnyRbe {
		if !rbeSupported {
			return nil, fmt.Errorf("Sorry, RBE is only supported on linux-x64 at this time.")
		}
		if !canUseRbe {
			fmt.Printf("Note: RBE is not publicly accessible at this time.")
		}
	}

	var (
		// These variables eventually represent our final decisions of whether
		// to use rbe/goma/ccache, since the logic is somewhat convoluted.
		useCxxRbeFinal bool
		useGomaFinal   bool
		useCcacheFinal bool
	)

	// Check CCACHE_DIR if it is specified.
	if !(args.useCcache || args.noCcache) {
		if args.ccacheDir != "" {
			isDir, err := osmisc.IsDir(args.ccacheDir)
			if err != nil {
				return nil, fmt.Errorf("failed to check existence of $%s: %w", ccacheDirEnvVar, err)
			}
			if !isDir {
				return nil, fmt.Errorf("$%s=%s does not exist or is a regular file", ccacheDirEnvVar, args.ccacheDir)
			}
			useCcacheFinal = true
		}
	}

	// Goma is deprecated, so do not automatically enable it.
	if args.useGoma || args.gomaDir != "" {
		// User explicitly tries to enable Goma (deprecated).
		// Allow this for a limited time before removing this option.
		fmt.Printf("Goma is deprecated and shutting down. This will be disabled in the near future. See go/fuchsia-goma-shutdown-psa.")
		if rbeSupported && canUseRbe {
			fmt.Printf("On this platform, you can build C++ on RBE using '--cxx-rbe'.")
		}
		gomaAuth, err := isGomaAuthenticated(ctx, fx)
		if err != nil {
			return nil, err
		}
		if gomaAuth {
			useGomaFinal = true
		}
	} else if args.noGoma {
		useGomaFinal = false
	}

	// The old behavior enabled Goma by default, but now that Goma
	// is deprecated, we replace it by enabling --cxx-rbe by default
	// only on supported platforms.
	if args.enableCxxRbe {
		useCxxRbeFinal = true
	} else if !args.disableCxxRbe {
		if rbeSupported && canUseRbe && !args.useGoma && !args.useCcache {
			useCxxRbeFinal = true
		}
	}

	if !useGomaFinal {
		if args.useCcache {
			useCcacheFinal = true
		} else if args.noCcache {
			useCcacheFinal = false
		}
	}

	gnArgs := args.gnArgs
	if useCcacheFinal {
		gnArgs = append(gnArgs, "use_ccache=true")
	}

	// fint already translates the *_rbe_enable variables into GN args.

	if args.netboot {
		gnArgs = append(gnArgs, "enable_netboot=true")
	}

	if args.includeClippy {
		gnArgs = append(gnArgs, "include_clippy=true")
	}

	hostLabels := args.hostLabels
	if args.cargoTOMLGen {
		hostLabels = append(hostLabels, "//build/rust:cargo_toml_gen")
	}

	return &fintpb.Static{
		Board:               boardPath,
		Product:             productPath,
		Optimize:            optimize,
		BasePackages:        args.basePackages,
		CachePackages:       args.cachePackages,
		UniversePackages:    args.universePackages,
		HostLabels:          hostLabels,
		DeveloperTestLabels: args.testLabels,
		Variants:            variants,
		GnArgs:              gnArgs,
		UseGoma:             useGomaFinal,
		RustRbeEnable:       args.enableRustRbe,
		CxxRbeEnable:        useCxxRbeFinal,
		LinkRbeEnable:       args.enableLinkRbe,
		BazelRbeEnable:      args.enableBazelRbe,
		IdeFiles:            args.ideFiles,
		JsonIdeScripts:      args.jsonIDEScripts,
		ExportRustProject:   true,
	}, nil
}

// fuzzerVariants produces the variants for enabling a sanitizer on fuzzers.
func fuzzerVariants(sanitizer string) []string {
	return []string{
		fmt.Sprintf(`{variant="%s-fuzzer" target_type=["fuzzed_executable"]}`, sanitizer),
		// TODO(https://fxbug.dev/42113953): Fuzzers need a version of libfdio.so that is sanitized,
		// but doesn't collect coverage data.
		fmt.Sprintf(`{variant="%s" label=["//sdk/lib/fdio"]}`, sanitizer),
	}
}

// findGNIFile returns the relative path to a board or product file in a
// checkout, given a basename. It checks the root of the checkout as well as
// each vendor/* directory for a file matching "<dirname>/<basename>.gni", e.g.
// "boards/core.gni".
func findGNIFile(checkoutDir, dirname, basename string) (string, error) {
	dirs, err := filepath.Glob(filepath.Join(checkoutDir, "vendor", "*", dirname))
	if err != nil {
		return "", err
	}
	// Prefer vendor products in alphabetical order.
	sort.Strings(dirs)
	dirs = append(dirs, filepath.Join(checkoutDir, dirname))

	for _, dir := range dirs {
		path := filepath.Join(dir, fmt.Sprintf("%s.gni", basename))
		exists, err := osmisc.FileExists(path)
		if err != nil {
			return "", err
		}
		if exists {
			return filepath.Rel(checkoutDir, path)
		}
	}

	return "", fmt.Errorf("no such file %s.gni", basename)
}

func allEnvVars() map[string]string {
	env := make(map[string]string)
	for _, keyAndValue := range os.Environ() {
		parts := strings.SplitN(keyAndValue, "=", 2)
		key, val := parts[0], parts[1]
		env[key] = val
	}
	return env
}

func canAccessRbe(checkoutDir string) (bool, error) {
	// Note: This is not perfect because it does not actually check against ACL
	// but it avoids the problem of external developers accidentally
	// configuring use of RBE.
	cmd := exec.Command("git", "remote", "-v")
	cmd.Dir = checkoutDir + "/integration"
	out, err := cmd.Output()
	if err != nil {
		return false, err
	}
	lines := strings.Split(string(out), "\n")
	if len(lines) < 1 {
		return false, fmt.Errorf("Failed to read 'git remote -v'")
	}
	// Check all remotes.  If any have SSO access, then assume user
	// can access RBE.
	for _, line := range lines {
		fields := strings.Fields(line)
		// Expect lines like:
		//   "origin	sso://.../integration (fetch)"
		// or
		//   "origin	https://.../integration (fetch)"
		if len(fields) >= 2 && strings.HasPrefix(fields[1], "sso://") {
			return true, nil
		}
	}
	return false, nil
}

func isGomaAuthenticated(ctx context.Context, fx fxRunner) (bool, error) {
	if err := fx.runWithNoStdio(ctx, "goma_auth", "info"); err != nil {
		var exitError *exec.ExitError
		if errors.As(err, &exitError) {
			// The command failed, which probably means the user isn't logged
			// into Goma.
			return false, nil
		}
		return false, err
	}
	return true, nil
}
