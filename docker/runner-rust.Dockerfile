# Rust runner image. Alpine's packaged toolchain is deliberately generic: if a
# target repository pins a toolchain, extend this image with rustup for that
# version. Mount /home/nuwa/.cargo to persist the registry/build cache.
FROM nuwa-runner-base:local

USER root
RUN apk add --no-cache rust cargo build-base pkgconf openssl-dev
USER nuwa
