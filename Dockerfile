# Minimal runtime image
FROM gcr.io/distroless/static-debian12:nonroot
COPY mt /mt
EXPOSE 8080
ENTRYPOINT ["/mt", "mcp", "--addr", "0.0.0.0:8080"]
