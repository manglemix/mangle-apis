#!/bin/bash
cargo build --release && \
upx -9 -q -o../persistent/target/release/bola-api-c --brute ../persistent/target/release/bola-api && \
rm ../persistent/target/release/bola-api && \
mv ../persistent/target/release/bola-api-c ../persistent/target/release/bola-api