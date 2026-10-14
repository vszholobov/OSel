module osel/network-fixture

go 1.27

require (
	github.com/gorilla/mux v1.8.1
	github.com/gorilla/websocket v1.5.1
	tetrisServer v0.0.0
)

require (
	github.com/beorn7/perks v1.0.1 // indirect
	github.com/cespare/xxhash/v2 v2.2.0 // indirect
	github.com/matttproud/golang_protobuf_extensions/v2 v2.0.0 // indirect
	github.com/prometheus/client_golang v1.18.0 // indirect
	github.com/prometheus/client_model v0.5.0 // indirect
	github.com/prometheus/common v0.45.0 // indirect
	github.com/prometheus/procfs v0.12.0 // indirect
	github.com/sirupsen/logrus v1.9.3 // indirect
	github.com/vszholobov/tetrisLib v0.0.0 // indirect
	golang.org/x/net v0.17.0 // indirect
	golang.org/x/sys v0.15.0 // indirect
	google.golang.org/protobuf v1.31.0 // indirect
)

replace tetrisServer => ../../apps/tetris/server

replace github.com/vszholobov/tetrisLib => ../../apps/tetris/lib
