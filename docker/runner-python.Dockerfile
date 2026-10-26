# Python runner image. Mount /home/nuwa/.cache/pip for the pip cache.
FROM nuwa-runner-base:local

USER root
RUN apk add --no-cache python3 py3-pip
USER nuwa
