FROM alpine:3.19

RUN apk add --no-cache \
    bash \
    coreutils \
    e2fsprogs \
    findutils \
    nodejs \
    npm \
    python3
