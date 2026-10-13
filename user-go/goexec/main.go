// goexec checks child processes, replacement images, descriptor inheritance and pipes.
package main

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"syscall"
)

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "goexec: "+format+"\n", args...)
	os.Exit(1)
}

func main() {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "child37":
			if len(os.Args) != 3 || os.Args[2] != "argument with spaces" || os.Getenv("OSEL_CHILD") != "inherited value" {
				fail("child argv/env mismatch: %q, %q", os.Args, os.Getenv("OSEL_CHILD"))
			}
			fmt.Print("child output")
			os.Exit(37)
		case "copy":
			if _, err := io.Copy(os.Stdout, os.Stdin); err != nil {
				fail("copy: %v", err)
			}
			return
		default:
			fail("unexpected child mode")
		}
	}

	// PATH lookup is the same path used by Tetris's exec.Command("clear").
	for i := 0; i < 8; i++ {
		output, err := exec.Command("clear").Output()
		if err != nil || string(output) != "\x1b[H\x1b[2J" {
			fail("clear #%d: %q, %v", i, output, err)
		}
	}
	fmt.Println("goexec clear ok")

	cmd := exec.Command("/bin/goexec", "child37", "argument with spaces")
	cmd.Env = []string{"GODEBUG=asyncpreemptoff=1", "OSEL_CHILD=inherited value"}
	output, err := cmd.Output()
	var exitError *exec.ExitError
	if !errors.As(err, &exitError) || exitError.ExitCode() != 37 || string(output) != "child output" {
		fail("nonzero child: %q, %v", output, err)
	}
	fmt.Println("goexec argv env exit ok")

	for i := 0; i < 4; i++ {
		err = exec.Command("/bin/does-not-exist").Run()
		if !errors.Is(err, syscall.ENOENT) {
			fail("missing executable: %v", err)
		}
	}
	fmt.Println("goexec failed exec ok")

	// More bytes than a pipe holds exercises writer blocking and Go's epoll reader.
	payload := bytes.Repeat([]byte("pipe round trip\n"), 4096)
	cmd = exec.Command("/bin/goexec", "copy")
	cmd.Stdin = bytes.NewReader(payload)
	output, err = cmd.Output()
	if err != nil || !bytes.Equal(output, payload) {
		fail("pipe round trip: got %d of %d bytes, %v", len(output), len(payload), err)
	}
	fmt.Println("goexec pipes ok")
	fmt.Println("goexec done")
}
