// Real HTTP and two WebSocket players against the repository's Tetris server.
package main

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"github.com/gorilla/websocket"
	lib "github.com/vszholobov/tetrisLib"
	"io"
	"net"
	"net/http"
	"os"
	"syscall"
	"time"
)

func must(err error) {
	if err != nil {
		panic(err)
	}
}
func state(c *websocket.Conn, kind lib.FieldType) []byte {
	must(c.SetReadDeadline(time.Now().Add(5 * time.Second)))
	for {
		_, data, err := c.ReadMessage()
		must(err)
		if len(data) != 40 {
			panic("invalid game packet")
		}
		if lib.FieldType(data[0]) == kind {
			return data
		}
	}
}
func syscallChecks() {
	if _, err := syscall.Socket(syscall.AF_INET6, syscall.SOCK_STREAM, 0); err != syscall.EAFNOSUPPORT {
		panic("IPv6 accepted")
	}
	fd, err := syscall.Socket(syscall.AF_INET, syscall.SOCK_STREAM|syscall.SOCK_NONBLOCK|syscall.SOCK_CLOEXEC, 0)
	must(err)
	var stat syscall.Stat_t
	must(syscall.Fstat(fd, &stat))
	if stat.Mode&syscall.S_IFMT != syscall.S_IFSOCK {
		panic("socket stat type incorrect")
	}
	_, _, errno := syscall.RawSyscall(syscall.SYS_CONNECT, uintptr(fd), 0, 16)
	if errno != syscall.EFAULT {
		panic("invalid socket address accepted")
	}
	_, _, errno = syscall.RawSyscall6(syscall.SYS_GETSOCKOPT, uintptr(fd), syscall.SOL_SOCKET, syscall.SO_ERROR, 0, 0, 0)
	if errno != syscall.EFAULT {
		panic("invalid getsockopt pointer accepted")
	}
	duplicate, err := syscall.Dup(fd)
	must(err)
	must(syscall.Close(fd))
	must(syscall.Close(duplicate))
	var sockets []int
	for i := 0; i < 64; i++ {
		fd, err := syscall.Socket(syscall.AF_INET, syscall.SOCK_STREAM, 0)
		must(err)
		sockets = append(sockets, fd)
	}
	if _, err := syscall.Socket(syscall.AF_INET, syscall.SOCK_STREAM, 0); err != syscall.EMFILE {
		panic("socket limit missing")
	}
	for _, fd := range sockets {
		must(syscall.Close(fd))
	}
	fmt.Println("NETCHECK SOCKET ERRORS AND LIMIT")
}
func main() {
	syscallChecks()
	configBytes, err := os.ReadFile("/bin/network-test-address")
	must(err)
	address := string(configBytes)
	hosts, err := net.LookupHost("tetris.test")
	must(err)
	if len(hosts) != 1 || hosts[0] != "10.0.2.2" {
		panic("ramdisk hosts lookup failed")
	}
	_, port, err := net.SplitHostPort(address)
	must(err)
	address = "tetris.test:" + port
	fmt.Println("NETCHECK HOSTS")
	// Failed connect must wake the Go poller, with SO_ERROR, rather than hang.
	c, err := net.DialTimeout("tcp4", "10.0.2.2:1", 2*time.Second)
	if err == nil {
		c.Close()
		panic("closed port accepted a connection")
	}
	fmt.Println("NETCHECK REFUSED")
	client := &http.Client{Timeout: 10 * time.Second}
	payload := bytes.Repeat([]byte("TCP backpressure and partial writes\n"), 8192)
	response, err := client.Post("http://"+address+"/echo", "application/octet-stream", bytes.NewReader(payload))
	must(err)
	echoed, err := io.ReadAll(response.Body)
	must(err)
	must(response.Body.Close())
	if !bytes.Equal(payload, echoed) {
		panic("large TCP transfer corrupted")
	}
	fmt.Println("NETCHECK LARGE TRANSFER")
	for round := 0; round < 3; round++ {
		request, err := http.NewRequest("GET", "http://"+address+"/session", nil)
		must(err)
		request.Header.Set("X-Client-Version", "3.0.0")
		response, err := client.Do(request)
		must(err)
		body, err := io.ReadAll(response.Body)
		must(err)
		must(response.Body.Close())
		if response.StatusCode != 200 || len(body) == 0 {
			panic("session list failed")
		}
	}
	response, err = client.Get("http://" + address + "/session/create")
	must(err)
	var session struct {
		ID int64 `json:"sessionId"`
	}
	must(json.NewDecoder(response.Body).Decode(&session))
	must(response.Body.Close())
	url := fmt.Sprintf("ws://%s/session/connect/%d", address, session.ID)
	dialer := websocket.Dialer{HandshakeTimeout: 5 * time.Second}
	first, _, err := dialer.Dial(url, nil)
	must(err)
	defer first.Close()
	second, _, err := dialer.Dial(url, nil)
	must(err)
	defer second.Close()
	// Read the peer continuously from the first packet. Filtering for its own field
	// before comparison would discard earlier enemy snapshots under host load.
	peerStates := make(chan []byte, 256)
	peerErrors := make(chan error, 1)
	go func() {
		for {
			_, data, err := second.ReadMessage()
			if err != nil {
				peerErrors <- err
				return
			}
			if len(data) != 40 {
				peerErrors <- fmt.Errorf("invalid peer packet")
				return
			}
			peerStates <- data
		}
	}()
	own := state(first, lib.Self)
	_ = state(first, lib.Enemy)
	if binary.BigEndian.Uint16(own[35:37]) != 0 {
		panic("unexpected score")
	}
	must(first.WriteMessage(websocket.TextMessage, []byte{byte(lib.MoveDown)}))
	after := state(first, lib.Self)
	if string(own[2:34]) == string(after[2:34]) {
		panic("server did not apply movement")
	}
	matched, peerSelf := false, false
	deadline := time.NewTimer(5 * time.Second)
	for !matched || !peerSelf {
		select {
		case seen := <-peerStates:
			if lib.FieldType(seen[0]) == lib.Self {
				peerSelf = true
			}
			if lib.FieldType(seen[0]) == lib.Enemy && bytes.Equal(after[2:34], seen[2:34]) {
				matched = true
			}
		case err := <-peerErrors:
			panic(err)
		case <-deadline.C:
			panic("players disagree about the field")
		}
	}
	deadline.Stop()
	fmt.Println("NETCHECK GAME: two players, self/enemy fields, confirmed movement")
	first.SetPingHandler(func(data string) error {
		return first.WriteControl(websocket.PongMessage, []byte(data), time.Now().Add(time.Second))
	})
	must(first.WriteControl(websocket.PingMessage, []byte("osel"), time.Now().Add(time.Second)))
	pong := false
	first.SetPongHandler(func(data string) error { pong = data == "osel"; return nil })
	for !pong {
		_ = state(first, lib.Self)
	}
	fmt.Println("NETCHECK PONG")
	client.CloseIdleConnections()
	fmt.Println("NETCHECK OK")
}
