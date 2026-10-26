# Node runner image. Mount /home/nuwa/.npm for the npm cache.
FROM nuwa-runner-base:local

USER root
RUN apk add --no-cache nodejs npm
USER nuwa
