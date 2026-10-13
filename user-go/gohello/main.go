// gohello is the end-to-end check of stage 2: a static Go binary that OSel runs through
// Linux system calls. It prints, runs goroutines that sleep, and exits 0.
package main

import (
	"fmt"
	"os"
	"sync"
	"time"
)

func main() {
	fmt.Println("hello from go")

	var wg sync.WaitGroup
	results := make([]int, 5)
	for i := 0; i < 5; i++ {
		wg.Add(1)
		go func(n int) {
			defer wg.Done()
			time.Sleep(time.Duration(n) * 5 * time.Millisecond)
			results[n] = n * n
		}(i)
	}
	wg.Wait()

	sum := 0
	for _, r := range results {
		sum += r
	}
	fmt.Println("sum", sum)

	time.Sleep(10 * time.Millisecond)
	fmt.Println("go done")

	// the exit code carries the goroutine result (0+1+4+9+16 = 30), so the kernel test
	// can check correctness without reading the output
	os.Exit(sum)
}
