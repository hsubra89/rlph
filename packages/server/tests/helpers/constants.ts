export const TEST_JWT_SECRET_RAW = "test-secret-that-is-at-least-32-bytes-long"

export const TEST_JWT_SECRET = new TextEncoder().encode(TEST_JWT_SECRET_RAW)
