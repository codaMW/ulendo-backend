FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    libsqlite3-0 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY ulendo-bin /app/ulendo
COPY migrations /app/migrations

RUN chmod +x /app/ulendo

EXPOSE 8080
CMD ["/app/ulendo"]
