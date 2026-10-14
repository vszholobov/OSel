// Runs the actual Tetris server handlers on a private, ephemeral host port.
package main

import (
	"flag"
	"fmt"
	"github.com/gorilla/mux"
	"github.com/gorilla/websocket"
	"io"
	"net"
	"net/http"
	"os"
	"tetrisServer/server"
	"time"
)

func main() {
	bot := flag.Bool("bot", false, "join each game with a local opponent")
	flag.Parse()
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		panic(err)
	}
	router := mux.NewRouter()
	router.HandleFunc("/echo", func(w http.ResponseWriter, r *http.Request) { defer r.Body.Close(); io.Copy(w, r.Body) })
	router.HandleFunc("/session", server.GetSessionsList)
	router.HandleFunc("/session/create", server.CreateSession)
	router.HandleFunc("/session/connect/{sessionId}", func(w http.ResponseWriter, r *http.Request) {
		server.ConnectToSession(w, r)
		if *bot && r.URL.Query().Get("opponent") != "1" {
			id := mux.Vars(r)["sessionId"]
			go func() {
				dialer := websocket.Dialer{HandshakeTimeout: 5 * time.Second}
				c, _, err := dialer.Dial("ws://"+listener.Addr().String()+"/session/connect/"+id+"?opponent=1", nil)
				if err != nil {
					fmt.Fprintln(os.Stderr, err)
					return
				}
				defer c.Close()
				for {
					if _, _, err := c.ReadMessage(); err != nil {
						return
					}
				}
			}()
		}
	})
	fmt.Printf("NET SERVER %s\n", listener.Addr())
	if err := http.Serve(listener, router); err != nil {
		panic(err)
	}
}
