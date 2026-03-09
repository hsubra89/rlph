# Run all checks (mirrors CI)
check: fmt-check lint test ts-build

# Format check
fmt-check:
    cargo fmt --all -- --check
    pnpm --filter '@brrr/*' run fmt:check

# Lint
lint:
    cargo clippy --all-targets --all-features -- -D warnings
    pnpm --filter '@brrr/*' run lint

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
    pnpm --filter '@brrr/*' run fmt

# TypeScript build
ts-build:
    pnpm --filter '@brrr/*' run build

# TypeScript dev server
ts-dev:
    pnpm --filter @brrr/server run dev
