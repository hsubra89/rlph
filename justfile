# Run all checks (mirrors CI)
check: fmt-check lint test ts-build

# Format check
fmt-check:
    cargo fmt --all -- --check

# Lint
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Unit + lib tests
test:
    cargo nextest run

# Integration tests (CI gate)
integration:
    cargo nextest run --profile integration -E 'binary(cli_binary)'

# Integration tests (full local sweep)
integration-all:
    cargo nextest run --profile integration

# Format (fix)
fmt:
    cargo fmt --all

# TypeScript build
ts-build:
    pnpm --filter '@brrr/*' run build

# TypeScript dev server
ts-dev:
    pnpm --filter @brrr/server run dev
