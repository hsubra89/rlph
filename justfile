# Run all checks (mirrors CI)
[parallel]
check: fmt-check lint test ts-build

# Format check
fmt-check:
    cargo fmt --all -- --check
    pnpm fmt:check

# Lint
lint:
    cargo clippy --all-targets --all-features -- -D warnings
    pnpm lint

# Unit + lib tests
test:
    cargo nextest run
    pnpm test

# Integration tests (CI gate)
integration:
    cargo nextest run --profile integration -E 'binary(cli_binary)'
    pnpm test:integration

# Integration tests (full local sweep)
integration-all:
    cargo nextest run --profile integration

# Format (fix)
fmt:
    cargo fmt --all
    pnpm fmt

# TypeScript build
ts-build:
    pnpm build

# TypeScript dev server
dev:
    pnpm dev

build:
    cargo build
    pnpm build