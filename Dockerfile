# ── Stage 1: Build ──────────────────────────────────────────────────
FROM python:3.13-slim AS builder

WORKDIR /build
COPY pyproject.toml README.md ./
COPY qm_core/ qm_core/
COPY qm_app.py ./
COPY gateway/ gateway/
COPY core_db/ core_db/
COPY cache_layer/ cache_layer/
COPY search_platform/ search_platform/
COPY vector_platform/ vector_platform/
COPY analytics_platform/ analytics_platform/
COPY storage/ storage/
COPY indexing/ indexing/
COPY pipelines/ pipelines/
COPY observability/ observability/
COPY sdk/ sdk/

RUN pip install --no-cache-dir --prefix=/install .

# ── Stage 2: Runtime ────────────────────────────────────────────────
FROM python:3.13-slim

LABEL maintainer="QM Team" \
      description="QM Database — Hybrid AI-Native DBMS" \
      version="1.0.0"

# Copy only the installed packages and entry-point script
COPY --from=builder /install /usr/local
COPY --from=builder /build/qm_app.py /app/qm_app.py
COPY --from=builder /build/qm_core /app/qm_core
COPY --from=builder /build/gateway /app/gateway

WORKDIR /app

# Default data directory (mount a volume in production)
ENV QM_DATA_DIR=/data/qm \
    QM_HOST=0.0.0.0 \
    QM_PORT=5433 \
    QM_LOG_LEVEL=INFO \
    QM_ADMIN_PASSWORD=changeme

EXPOSE 5433

VOLUME ["/data/qm"]

HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
    CMD python -c "import socket; s=socket.create_connection(('127.0.0.1',5433),2); s.close()" || exit 1

ENTRYPOINT ["qm-server"]
CMD ["start", \
     "--data-dir", "/data/qm", \
     "--host",     "0.0.0.0", \
     "--port",     "5433"]
