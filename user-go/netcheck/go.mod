module osel/netcheck

go 1.27

require (
	github.com/gorilla/websocket v1.5.1
	github.com/vszholobov/tetrisLib v0.0.0
)

require golang.org/x/net v0.17.0 // indirect

replace github.com/vszholobov/tetrisLib => ../../apps/tetris/lib
