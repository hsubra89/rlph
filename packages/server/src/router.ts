import { HttpRouter, HttpServerResponse } from "@effect/platform";
import { handleLogin } from "./auth/login.js";
import { authMiddleware } from "./auth/middleware.js";
import { handleRevoke } from "./auth/revoke.js";
import { handleWhoami } from "./auth/whoami.js";

export const router = HttpRouter.empty.pipe(
  HttpRouter.get("/health", HttpServerResponse.json({ status: "ok" })),
  HttpRouter.post("/auth/login", handleLogin),
  HttpRouter.get("/whoami", authMiddleware(handleWhoami)),
  HttpRouter.post("/auth/revoke", authMiddleware(handleRevoke)),
);
