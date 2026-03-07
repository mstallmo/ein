#!/bin/bash

ROOT_DIR=~/.ein
PLUGIN_INSTALL_DIR=${ROOT_DIR}/plugins

############################
# BUILD                    #
############################

echo "Building read plugin..."
cargo build --release -p ein_read --target wasm32-wasip2
echo "Done"

echo "Building write plugin..."
cargo build --release -p ein_write --target wasm32-wasip2
echo "Done"

############################
# Install                  #
############################


if [ ! -d "$PLUGIN_INSTALL_DIR" ]; then
    mkdir -p "$PLUGIN_INSTALL_DIR"
fi

echo "Installing read plugin..."
cp target/wasm32-wasip2/release/ein_read.wasm ~/.ein/plugins
echo "Done"

echo "Installing write plugin..."
cp target/wasm32-wasip2/release/ein_write.wasm ~/.ein/plugins
echo "Done"
