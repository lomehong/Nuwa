# Nuwa runner base image: the static musl binary plus the tools every runner
# needs. Language images add their toolchain on top.
#
# Build context is the repository root; `docker/build.sh` (or the CI docker
# job) copies the musl binary to `docker/nuwa` first.
FROM alpine:3.20

# Controllers often bind-mount a checkout owned by another uid; the container
# is single-purpose and already unprivileged.
RUN apk add --no-cache ca-certificates git openssh-client bash tzdata \
    && git config --system --add safe.directory '*'

COPY docker/nuwa /usr/local/bin/nuwa
RUN chmod 0755 /usr/local/bin/nuwa \
    && adduser -D -u 10001 nuwa \
    && mkdir -p /work /data /sessions \
    && chown -R nuwa:nuwa /work /data /sessions

USER nuwa
WORKDIR /work
ENTRYPOINT ["nuwa"]
