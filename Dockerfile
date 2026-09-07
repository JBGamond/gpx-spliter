# Lightweight Dockerfile for GPX Splitter (Caddy only)
# This Dockerfile assumes you've built the WASM module locally using ./build.sh
FROM caddy:2.9-alpine

WORKDIR /usr/share/caddy

# Copy the static assets (which now includes the pre-built static/pkg from your host)
ADD static/ /usr/share/caddy/

EXPOSE 80
CMD ["caddy", "file-server", "--root", "/usr/share/caddy"]
