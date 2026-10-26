# Go runner image. GOFLAGS/GOPATH default under /home/nuwa; mount
# /home/nuwa/go to persist the module/build cache across containers.
FROM nuwa-runner-base:local

USER root
RUN apk add --no-cache go
USER nuwa
