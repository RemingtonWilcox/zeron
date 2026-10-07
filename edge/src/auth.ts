/**
 * Edge auth: verify WorkOS AuthKit access-token JWTs (jose against the WorkOS
 * JWKS) before any DO forwarding or R2 access. WebSocket upgrades carry the
 * token as `?token=` (WS clients cannot always set headers); plain requests
 * use `Authorization: Bearer`.
 *
 * Workspace rooms (`ws/{orgId}`) authorize on the token's WorkOS organization
 * claim (`org_id`, present when the session was refreshed scoped to an org):
 * membership = claim equals the room's orgId.
 *
 * `AUTH_MODE: "token"` replaces WorkOS for a self-hosted single-user relay:
 * the bearer is a shared secret mapped to one configured user and org.
 */
import { createRemoteJWKSet, jwtVerify } from "jose";
import type { Env } from "./env";

export interface Verified {
  readonly userId: string;
  readonly sessionId?: string;
  /** WorkOS `org_id` claim — the org the caller's session is scoped to. */
  readonly orgId?: string;
}

const jwksCache = new Map<string, ReturnType<typeof createRemoteJWKSet>>();

const getJwks = (url: string) => {
  let jwks = jwksCache.get(url);
  if (!jwks) {
    jwks = createRemoteJWKSet(new URL(url));
    jwksCache.set(url, jwks);
  }
  return jwks;
};

export const bearerFromRequest = (request: Request): string | undefined => {
  const header = request.headers.get("authorization");
  if (header?.toLowerCase().startsWith("bearer ")) return header.slice(7).trim();
  const url = new URL(request.url);
  return url.searchParams.get("token") ?? undefined;
};

const sha256 = async (value: string): Promise<Uint8Array> =>
  new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));

/** Constant-time equality: comparing fixed-length digests leaks neither the
 * secret's length nor the position of the first mismatch. */
export const tokenMatches = async (presented: string, secret: string): Promise<boolean> => {
  const [a, b] = await Promise.all([sha256(presented), sha256(secret)]);
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
};

export const verifyToken = async (env: Env, token: string): Promise<Verified | undefined> => {
  if (env.AUTH_MODE === "token") {
    // Self-hosted single-user relay: one shared secret stands for one fixed
    // user and org, so every device lands in the same rooms.
    const { RELAY_TOKEN, RELAY_USER_ID, RELAY_ORG_ID } = env;
    if (!RELAY_TOKEN || !RELAY_USER_ID || !RELAY_ORG_ID) return undefined;
    if (!(await tokenMatches(token, RELAY_TOKEN))) return undefined;
    return { userId: RELAY_USER_ID, orgId: RELAY_ORG_ID };
  }
  if (env.AUTH_MODE === "dev") {
    // Dev mode mirrors the old apps/server: the bearer string IS the user id.
    // `userId@orgId` additionally carries a fake org claim so workspace-room
    // membership is exercisable locally (smoke tests).
    if (!token) return undefined;
    const at = token.indexOf("@");
    if (at > 0) return { userId: token.slice(0, at), orgId: token.slice(at + 1) };
    return { userId: token };
  }
  const issuer =
    env.WORKOS_ISSUER ?? `https://api.workos.com/user_management/${env.WORKOS_CLIENT_ID}`;
  const jwksUrl = env.WORKOS_JWKS_URL ?? `https://api.workos.com/sso/jwks/${env.WORKOS_CLIENT_ID}`;
  try {
    const { payload } = await jwtVerify(token, getJwks(jwksUrl), { issuer });
    if (typeof payload.sub !== "string" || payload.sub.length === 0) return undefined;
    return {
      userId: payload.sub,
      sessionId: typeof payload.sid === "string" ? payload.sid : undefined,
      orgId: typeof payload.org_id === "string" ? payload.org_id : undefined
    };
  } catch {
    return undefined;
  }
};

export const authenticate = async (env: Env, request: Request): Promise<Verified | undefined> => {
  const token = bearerFromRequest(request);
  if (!token) return undefined;
  return verifyToken(env, token);
};
