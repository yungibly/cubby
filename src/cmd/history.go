package cmd

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/spf13/cobra"
)

var (
	historyLimit  int
	historyAll    bool
	historyFilter string
)

var historyCmd = &cobra.Command{
	Use:       "history",
	Short:     "Show sync and restore history",
	ValidArgs: []string{"sync", "restore"},
	RunE:      runHistory,
}

func init() {
	historyCmd.Flags().IntVarP(&historyLimit, "count", "c", 20, "number of entries to show")
	historyCmd.Flags().BoolVarP(&historyAll, "all", "a", false, "show all entries")
	historyCmd.Flags().StringVarP(&historyFilter, "filter", "f", "", "filter by operation: sync or restore")
	rootCmd.AddCommand(historyCmd)
}

func logPath() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".local", "share", "cubby", "history.log")
}

func logEntry(op, rel string) {
	path := logPath()
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return
	}
	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644)
	if err != nil {
		return
	}
	defer f.Close()
	fmt.Fprintf(f, "%s\t%s\t%s\n", time.Now().Format(time.RFC3339), op, rel)
}

func runHistory(cmd *cobra.Command, args []string) error {
	if len(args) > 0 {
		historyFilter = args[0]
	}

	f, err := os.Open(logPath())
	if err != nil {
		if os.IsNotExist(err) {
			fmt.Println("no history yet.")
			return nil
		}
		return err
	}
	defer f.Close()

	var lines []string
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := scanner.Text()
		if historyFilter != "" && !strings.Contains(line, "\t"+historyFilter+"\t") {
			continue
		}
		lines = append(lines, line)
	}

	if len(lines) == 0 {
		fmt.Println("no history found.")
		return nil
	}

	if !historyAll {
		if historyLimit > 0 && len(lines) > historyLimit {
			lines = lines[len(lines)-historyLimit:]
		}
	}

	for _, line := range lines {
		parts := strings.SplitN(line, "\t", 3)
		if len(parts) != 3 {
			continue
		}
		t, err := time.Parse(time.RFC3339, parts[0])
		if err != nil {
			continue
		}
		op := parts[1]
		rel := parts[2]

		var opStr string
		switch op {
		case "sync":
			opStr = green("✓  sync   ")
		case "restore":
			opStr = cyan("↓  restore")
		default:
			opStr = op
		}

		fmt.Printf("%s  %s  %s\n",
			dim(t.Format("2006-01-02 15:04:05")),
			opStr,
			rel,
		)
	}

	fmt.Printf("\n%s\n", dim(strconv.Itoa(len(lines))+" entries shown"+func() string {
		if !historyAll && len(lines) == historyLimit {
			return " (use --all to see full history)"
		}
		return ""
	}()))

	return nil
}
