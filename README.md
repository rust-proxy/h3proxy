h3proxy — simple HTTP/3 proxy with CONNECT support

This repository contains a minimal HTTP/3 proxy server built with quinn + h3 + tokio.
It supports:
- forwarding regular HTTP requests to upstream (HTTP/1.1 via hyper)
- CONNECT tunneling (to proxy TLS connections) by opening a TCP connection to the target and relaying bytes

Usage (for testing):
1. Place cert.pem and key.pem (self-signed OK for testing) into the project root.
   Example (openssl):
     openssl req -x509 -newkey rsa:4096 -nodes -subj "/CN=localhost" -keyout key.pem -out cert.pem -days 365

2. Build and run:
   cargo run --release

3. Point an HTTP/3-capable client to the proxy on port 4433. For CONNECT tests you can use specialized clients or write a small HTTP/3 client.

Notes:
- This is a demo. For production use you must harden TLS validation, error handling, resource limits, and more.
