#!/bin/bash

TOOLS_DIR=~/.ein/plugins/tools
MODEL_CLIENTS_DIR=~/.ein/plugins/model_clients

############################
# BUILD                    #
############################

echo "Building bash plugin..."
cargo build --release -p ein_bash --target wasm32-wasip2
echo "Done"

echo "Building read plugin..."
cargo build --release -p ein_read --target wasm32-wasip2
echo "Done"

echo "Building write plugin..."
cargo build --release -p ein_write --target wasm32-wasip2
echo "Done"

echo "Building edit plugin..."
cargo build --release -p ein_edit --target wasm32-wasip2
echo "Done"

echo "Building openrouter model client plugin..."
cargo build --release -p ein_openrouter --target wasm32-wasip2
echo "Done"

############################
# Install                  #
############################

mkdir -p "$TOOLS_DIR" "$MODEL_CLIENTS_DIR"

echo "Installing bash plugin..."
cp target/wasm32-wasip2/release/ein_bash.wasm "$TOOLS_DIR"
echo "Done"

echo "Installing read plugin..."
cp target/wasm32-wasip2/release/ein_read.wasm "$TOOLS_DIR"
echo "Done"

echo "Installing write plugin..."
cp target/wasm32-wasip2/release/ein_write.wasm "$TOOLS_DIR"
echo "Done"

echo "Installing edit plugin..."
cp target/wasm32-wasip2/release/ein_edit.wasm "$TOOLS_DIR"
echo "Done"

echo "Installing openrouter model client plugin..."
cp target/wasm32-wasip2/release/ein_openrouter.wasm "$MODEL_CLIENTS_DIR"
echo "Done"
