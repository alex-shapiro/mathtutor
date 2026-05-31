# Minimal runtime image
FROM gcr.io/distroless/static-debian12:nonroot
COPY mathtutor /mathtutor
EXPOSE 8080
ENTRYPOINT ["/mathtutor", "mcp", "--addr", "0.0.0.0:8080"]
