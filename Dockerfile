FROM golang:1.22-alpine

RUN apk add --no-cache bash git

WORKDIR /app

ENV CGO_ENABLED=0

RUN mkdir -p /root/.config/cubby \
    && echo 'store = "/root/test-store"' > /root/.config/cubby/config.toml \
    && mkdir -p /root/test-store \
    && touch /root/test-store/example-file.txt

CMD ["/bin/bash"]
