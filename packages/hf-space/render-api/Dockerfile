FROM node:20-slim

# Create non-root user for HuggingFace Spaces
RUN useradd -m -u 1000 user
USER user
ENV HOME=/home/user PATH=/home/user/.local/bin:$PATH

WORKDIR /home/user/app

# Copy application files directly - no npm install needed
COPY --chown=user kernel-wasm ./kernel-wasm
COPY --chown=user server.mjs ./

# Verify files exist
RUN ls -la && ls -la kernel-wasm/

EXPOSE 7860

CMD ["node", "server.mjs"]

