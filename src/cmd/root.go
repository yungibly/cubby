package cmd

import (
	"bufio"
	"crypto/sha256"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/spf13/cobra"
	"github.com/spf13/viper"
)

// ── ANSI helpers ───────────────────────────────────────────────────────────────

const (
	colorReset  = "\033[0m"
	colorRed    = "\033[31m"
	colorGreen  = "\033[32m"
	colorYellow = "\033[33m"
	colorCyan   = "\033[36m"
	colorDim    = "\033[2m"
	colorBold   = "\033[1m"
)

func green(s string) string  { return colorGreen + s + colorReset }
func yellow(s string) string { return colorYellow + s + colorReset }
func red(s string) string    { return colorRed + s + colorReset }
func cyan(s string) string   { return colorCyan + s + colorReset }
func dim(s string) string    { return colorDim + s + colorReset }
func bold(s string) string   { return colorBold + s + colorReset }

// ── flags ──────────────────────────────────────────────────────────────────────

var (
	dryRun bool
	debug  bool
)

// ── root command ───────────────────────────────────────────────────────────────

var rootCmd = &cobra.Command{
	Use:   "keep <file|dir>...",
	Short: "Sync files to your storage directory",
	Long: `keep syncs files and directories from your home directory into a
mirrored structure inside your storage directory (~/.mydots by default),
preserving the full path relative to $HOME.

The storage directory can be configured in ~/.config/keep/config.toml:

  store = "~/.mydots"

Use "keep help <command>" for help on a specific command.`,
	Args:              cobra.MinimumNArgs(1),
	RunE:              runSync,
	DisableFlagParsing: false,
	ValidArgsFunction: func(cmd *cobra.Command, args []string, toComplete string) ([]string, cobra.ShellCompDirective) {
		return nil, cobra.ShellCompDirectiveDefault
	},
}

func Execute() {
	if err := rootCmd.Execute(); err != nil {
		os.Exit(1)
	}
}

func init() {
	cobra.OnInitialize(initConfig)

	rootCmd.PersistentFlags().BoolVar(&debug, "debug", false, "show debug information")
	rootCmd.Flags().BoolVarP(&dryRun, "dry-run", "n", false, "simulate without making changes")

	// hide flags we don't want cluttering help output
	rootCmd.PersistentFlags().MarkHidden("debug")

	rootCmd.AddCommand(restoreCmd)
	rootCmd.AddCommand(completionCmd)
}

// ── config ─────────────────────────────────────────────────────────────────────

func initConfig() {
	home, _ := os.UserHomeDir()
	viper.AddConfigPath(filepath.Join(home, ".config", "keep"))
	viper.SetConfigName("config")
	viper.SetConfigType("toml")
	viper.SetDefault("store", "~/.mydots")
	viper.ReadInConfig()
}

func resolvedStoreDir() string {
	d := viper.GetString("store")
	if strings.HasPrefix(d, "~/") {
		home, _ := os.UserHomeDir()
		d = filepath.Join(home, d[2:])
	}
	return d
}

// ── output ─────────────────────────────────────────────────────────────────────

func debugf(format string, args ...interface{}) {
	if debug {
		fmt.Fprintf(os.Stderr, cyan("[debug]")+" "+format+"\n", args...)
	}
}

func errorf(format string, args ...interface{}) {
	fmt.Fprintf(os.Stderr, red("✗")+"  "+format+"\n", args...)
}

func printSync(rel, store string) {
	fmt.Printf("%s  %s  %s\n",
		green("✓"),
		bold("~/"+rel),
		dim("→  "+store+"/"+rel),
	)
}

func printRestore(rel, store string) {
	fmt.Printf("%s  %s  %s\n",
		cyan("↓"),
		bold(store+"/"+rel),
		dim("→  ~/"+rel),
	)
}

func printDryRunHeader(src, dst string) {
	fmt.Printf("%s  %s  %s\n",
		yellow("~"),
		bold(src),
		dim("→  "+dst),
	)
}

func printFileChange(symbol, color, file, label string) {
	fmt.Printf("   %s  %-40s %s\n",
		color+symbol+colorReset,
		file,
		dim("("+label+")"),
	)
}

func confirm(prompt string) bool {
	fmt.Printf("%s %s ", yellow("?"), prompt+" [y/N]")
	r, _ := bufio.NewReader(os.Stdin).ReadString('\n')
	return strings.TrimSpace(strings.ToLower(r)) == "y"
}

// ── file operations ────────────────────────────────────────────────────────────

func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()

	info, err := in.Stat()
	if err != nil {
		return err
	}

	out, err := os.OpenFile(dst, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, info.Mode())
	if err != nil {
		return err
	}
	defer out.Close()

	_, err = io.Copy(out, in)
	return err
}

func hashFile(path string) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return nil, err
	}
	return h.Sum(nil), nil
}

func rsyncItemizeOutput(line string) {
	if len(line) < 10 {
		return
	}
	code := line[:10]
	file := strings.TrimSpace(line[10:])
	if file == "" {
		return
	}
	switch {
	case strings.HasPrefix(code, ">f+++++++++"):
		printFileChange("+", colorGreen, file, "new")
	case strings.HasPrefix(code, ">f"):
		printFileChange("~", colorYellow, file, "modified")
	case strings.HasPrefix(code, "*deleting"):
		printFileChange("-", colorRed, file, "deleted")
	case strings.HasPrefix(code, "cd+++++++++"):
		printFileChange("+", colorGreen, file, "new dir")
	}
}

func syncDir(src, dst string, dry bool) error {
	args := []string{"-a", "--delete", "--checksum", "--itemize-changes"}
	if dry {
		args = append(args, "--dry-run")
	}
	args = append(args, src+"/", dst+"/")
	debugf("rsync %s", strings.Join(args, " "))

	cmd := exec.Command("rsync", args...)
	out, err := cmd.Output()
	if err != nil {
		return err
	}
	for _, line := range strings.Split(string(out), "\n") {
		rsyncItemizeOutput(line)
	}
	return nil
}

func dryRunFile(src, dst string) {
	_, err := os.Stat(dst)
	if os.IsNotExist(err) {
		printFileChange("+", colorGreen, filepath.Base(src), "new")
		return
	}
	srcHash, err1 := hashFile(src)
	dstHash, err2 := hashFile(dst)
	if err1 != nil || err2 != nil {
		printFileChange("?", colorYellow, filepath.Base(src), "could not compare")
		return
	}
	if string(srcHash) == string(dstHash) {
		printFileChange("=", colorDim, filepath.Base(src), "unchanged")
	} else {
		printFileChange("~", colorYellow, filepath.Base(src), "modified")
	}
}

// ── sync ───────────────────────────────────────────────────────────────────────

func runSync(cmd *cobra.Command, args []string) error {
	home, _ := os.UserHomeDir()
	store := resolvedStoreDir()
	cwd, _ := os.Getwd()
	debugf("home=%s store=%s cwd=%s", home, store, cwd)

	for _, target := range args {
		abs, err := filepath.Abs(target)
		if err != nil {
			errorf("cannot resolve %s: %v", target, err)
			continue
		}
		debugf("target=%s abs=%s", target, abs)

		if !strings.HasPrefix(abs, home+string(filepath.Separator)) {
			errorf("%s is outside $HOME", abs)
			continue
		}

		rel := strings.TrimPrefix(abs, home+string(filepath.Separator))
		dst := filepath.Join(store, rel)
		debugf("rel=%s dst=%s", rel, dst)

		info, err := os.Stat(abs)
		if err != nil {
			errorf("%s: %v", target, err)
			continue
		}

		if info.IsDir() {
			if dryRun {
				printDryRunHeader("~/"+rel, store+"/"+rel)
				syncDir(abs, dst, true)
				continue
			}
			if err := os.MkdirAll(dst, 0755); err != nil {
				errorf("%v", err)
				continue
			}
			if err := syncDir(abs, dst, false); err != nil {
				errorf("sync failed for %s: %v", target, err)
				continue
			}
		} else {
			if dryRun {
				printDryRunHeader("~/"+rel, store+"/"+rel)
				dryRunFile(abs, dst)
				continue
			}
			if err := os.MkdirAll(filepath.Dir(dst), 0755); err != nil {
				errorf("%v", err)
				continue
			}
			if err := copyFile(abs, dst); err != nil {
				errorf("copy failed for %s: %v", target, err)
				continue
			}
		}

		printSync(rel, store)
		logEntry("sync", rel)
	}

	return nil
}
