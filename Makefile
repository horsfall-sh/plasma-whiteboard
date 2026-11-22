# Build the page to WebAssembly, then the relay (which embeds the page).
PAGE_WASM := page/target/wasm32-unknown-unknown/release/wb_page.wasm
# The wasm-bindgen-cli that writes web/page.js must match the wasm-bindgen crate
# pinned in page/Cargo.toml exactly, or the glue and the module disagree.
WASM_BINDGEN := 0.2.100

.PHONY: ship all wasm relay test run linux clean

all: relay

wasm:
	@v=$$(wasm-bindgen --version 2>/dev/null | cut -d' ' -f2); \
	  test "$$v" = "$(WASM_BINDGEN)" || { \
	  echo "wasm-bindgen-cli $(WASM_BINDGEN) is needed (found: $${v:-none}). Install it with:" >&2; \
	  echo "  cargo install wasm-bindgen-cli --version $(WASM_BINDGEN) --locked" >&2; exit 1; }
	cd page && cargo build --release --locked --target wasm32-unknown-unknown
	wasm-bindgen --target web --no-typescript --out-dir web --out-name page $(PAGE_WASM)

relay: wasm
	cd relay && cargo build --release

test: wasm
	cd smooth && cargo test
	cd crumple && cargo test
	cd relay && cargo test
	cd page && cargo test
	node --check web/boot.js

# Local run on http://127.0.0.1:8787, serving web/ from disk so page edits show on reload.
run: wasm
	cd relay && WB_WEB_DIR=../web cargo run

# Static Linux x86-64 binary for the server, built in a container (needs Docker).
# The source goes in and the binary comes out through pipes, so Docker needs no
# access to this folder. The image is pinned by digest and the build by
# Cargo.lock, so the same commit builds the same way; to move to a newer Rust,
# change the tag and the digest together.
RUST_IMAGE = rust:1.96.0-alpine@sha256:f87aa870663e2b57ec8c69de82c7eedf7383bee987eef7612c0359635eaadb41
linux: wasm
	mkdir -p relay/target-linux/release
	tar -c --exclude target --exclude target-linux relay web | \
	  docker run --rm -i --platform linux/amd64 -v wb-cargo-cache:/usr/local/cargo/registry $(RUST_IMAGE) \
	  sh -c "apk add --no-cache musl-dev >/dev/null && mkdir /src && tar -x -C /src && cd /src/relay && cargo build --release --locked >&2 && cat target/release/wb-relay" \
	  > relay/target-linux/release/wb-relay.tmp
	mv relay/target-linux/release/wb-relay.tmp relay/target-linux/release/wb-relay
	chmod 755 relay/target-linux/release/wb-relay
	@ls -lh relay/target-linux/release/wb-relay

clean:
	rm -rf smooth/target crumple/target page/target relay/target relay/target-linux web/page.js web/page_bg.wasm

# Test, build for the server and deploy to the live board. Run after every
# change so the public board always matches the latest commit.
# Set HOST (ssh target, e.g. root@server) and DOMAIN (public hostname) when running:
#   make ship HOST=root@server DOMAIN=whiteboard.example.org
HOST ?=
DOMAIN ?= whiteboard.example.com
# Only a committed tree ships, so what is live is always a commit.
clean-tree:
	@test -z "$$(git status --porcelain)" || { echo "uncommitted changes: commit or stash them before shipping" >&2; git status --short >&2; exit 1; }

ship: clean-tree test linux
	@test -n "$(HOST)" || { echo "HOST is not set: run make ship HOST=user@server DOMAIN=your.domain" >&2; exit 1; }
	sh deploy/deploy.sh $(HOST)
	@curl -fsS -o /dev/null -w "live: %{http_code}\n" https://$(DOMAIN)/healthz
