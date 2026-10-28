FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && apt-get clean

# The glibc binary (built by the release workflow, against an older glibc than the image's;
# Dockerfile.source builds it from the sources instead): musl's allocator serves one thread at a
# time, which costs most of the throughput of a server with several
COPY super /app/super

WORKDIR /app

# 443/udp: HTTP/3, when `server.http3` is on
EXPOSE 80 443 443/udp

ENTRYPOINT ["/app/super"]
