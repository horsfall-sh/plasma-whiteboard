# Build the stroke simplifier to WebAssembly, then the relay (which embeds the page).
WASM := smooth/target/wasm32-unknown-unknown/release/wb_smooth.wasm

.PHONY: all wasm relay test run linux clean

all: relay

wasm:
	cd smooth && cargo build --release --target wasm32-unknown-unknown
	cp $(WASM) web/smooth.wasm

relay: wasm
	cd relay && cargo build --release

test: wasm
	cd smooth && cargo test
	cd relay && cargo test
	node --check web/app.js

# Local run on http://127.0.0.1:8787, serving web/ from disk so page edits show on reload.
run: wasm
	cd relay && WB_WEB_DIR=../web cargo run

# Static Linux x86-64 binary for the server, built in a container (needs Docker).
linux: wasm
	docker run --rm --platform linux/amd64 -v "$(CURDIR)":/src -w /src/relay \
	  -e CARGO_TARGET_DIR=/src/relay/target-linux rust:1-alpine \
	  sh -c "apk add --no-cache musl-dev >/dev/null && cargo build --release"
	@ls -lh relay/target-linux/release/wb-relay

clean:
	rm -rf smooth/target relay/target relay/target-linux web/smooth.wasm
