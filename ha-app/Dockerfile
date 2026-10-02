# The Screeny app image is the Studio's own image, built from the repository
# root: `docker build --target app -t ghcr.io/gotwalt/screeny-studio .` (card
# 360 publishes it, and `config.yaml`'s `image:` is what the Supervisor pulls,
# so this file is not what an install uses). It exists so the app folder is
# complete for a local build, which can pass the image it wants as BUILD_FROM.
ARG BUILD_FROM=ghcr.io/gotwalt/screeny-studio:latest
FROM ${BUILD_FROM}

LABEL \
    org.opencontainers.image.title="Home Assistant App: Screeny" \
    org.opencontainers.image.source="https://github.com/gotwalt/screeny" \
    org.opencontainers.image.licenses="MIT"
