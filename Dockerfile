FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && apt-get clean

# A static (musl) binary, so it runs regardless of the image's glibc (built by the release
# workflow; Dockerfile.source builds it from the sources instead)
COPY super /app/super

WORKDIR /app

EXPOSE 80 443

ENTRYPOINT ["/app/super"]
