# Build the stroke simplifier to WebAssembly, then the relay (which embeds the page).
WASM := smooth/target/wasm32-unknown-unknown/release/wb_smooth.wasm

.PHONY: all wasm relay test run linux clean

all: relay

wasm:
	cd smooth && cargo build --release --target wasm32-unknown-unknown
	cp $(WASM) web/smooth.wasm
	cd crumple && cargo build --release --target wasm32-unknown-unknown
	cp crumple/target/wasm32-unknown-unknown/release/wb_crumple.wasm web/crumple.wasm

relay: wasm
	cd relay && cargo build --release

test: wasm
	cd smooth && cargo test
	cd crumple && cargo test
	cd relay && cargo test
	node --check web/app.js

# Local run on http://127.0.0.1:8787, serving web/ from disk so page edits show on reload.
run: wasm
	cd relay && WB_WEB_DIR=../web cargo run

# Static Linux x86-64 binary for the server, built in a container (needs Docker).
# The source goes in and the binary comes out through pipes, so Docker needs no
# access to this folder.
linux: wasm
	mkdir -p relay/target-linux/release
	tar -c --exclude target --exclude target-linux relay web | \
	  docker run --rm -i --platform linux/amd64 -v wb-cargo-cache:/usr/local/cargo/registry rust:1-alpine \
	  sh -c "apk add --no-cache musl-dev >/dev/null && mkdir /src && tar -x -C /src && cd /src/relay && cargo build --release >&2 && cat target/release/wb-relay" \
	  > relay/target-linux/release/wb-relay.tmp
	mv relay/target-linux/release/wb-relay.tmp relay/target-linux/release/wb-relay
	chmod 755 relay/target-linux/release/wb-relay
	@ls -lh relay/target-linux/release/wb-relay

clean:
	rm -rf smooth/target crumple/target relay/target relay/target-linux web/smooth.wasm web/crumple.wasm
