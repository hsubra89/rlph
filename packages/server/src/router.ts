import { HttpRouter } from "@effect/platform"
import { handleLogin } from "./auth/login.js"
import { authMiddleware } from "./auth/middleware.js"
import { handleHealth } from "./health.js"
import { handleRevoke } from "./auth/revoke.js"
import { handleWhoami } from "./auth/whoami.js"

export const router = HttpRouter.empty.pipe(
  HttpRouter.get("/health", handleHealth),
  HttpRouter.post("/auth/login", handleLogin),
  HttpRouter.get("/whoami", authMiddleware(handleWhoami)),
  HttpRouter.post("/auth/revoke", authMiddleware(handleRevoke)),
)
