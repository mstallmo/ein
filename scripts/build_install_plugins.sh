#!/bin/bash

ROOT_DIR=~/.ein
PLUGIN_INSTALL_DIR=${ROOT_DIR}/plugins

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

############################
# Install                  #
############################


if [ ! -d "$PLUGIN_INSTALL_DIR" ]; then
    mkdir -p "$PLUGIN_INSTALL_DIR"
fi

echo "Installing bash plugin..."
cp target/wasm32-wasip2/release/ein_bash.wasm ~/.ein/plugins
echo "Done"

echo "Installing read plugin..."
cp target/wasm32-wasip2/release/ein_read.wasm ~/.ein/plugins
echo "Done"

echo "Installing write plugin..."
cp target/wasm32-wasip2/release/ein_write.wasm ~/.ein/plugins
echo "Done"

echo "Installing edit plugin..."
cp target/wasm32-wasip2/release/ein_edit.wasm ~/.ein/plugins
echo "Done"
