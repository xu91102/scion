FROM node:24.22.1-bookworm-slim
RUN mkdir -p /etc/ssl/certs \
    && node -e 'process.stdout.write(require("node:tls").rootCertificates.join("\n")+"\n")' > /etc/ssl/certs/ca-certificates.crt \
    && npm install -g @openai/codex@0.160.1
ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
COPY --chmod=755 scion-agent /scion-agent
