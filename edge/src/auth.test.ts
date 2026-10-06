import { describe, expect, it } from "vitest";
import { authenticate, tokenMatches, verifyToken } from "./auth";
import type { Env } from "./env";

const SECRET = "4f1c0d2a9b7e6f5a3c8d1e0f2a4b6c8d";

const tokenEnv = (over: Partial<Env> = {}): Env =>
  ({
    AUTH_MODE: "token",
    WORKOS_CLIENT_ID: "",
    RELAY_TOKEN: SECRET,
    RELAY_USER_ID: "owner",
    RELAY_ORG_ID: "personal",
    ...over
  }) as Env;

describe("token auth mode", () => {
  it("maps the shared secret to the configured user and org", async () => {
    expect(await verifyToken(tokenEnv(), SECRET)).toEqual({ userId: "owner", orgId: "personal" });
  });

  it("refuses any other bearer, including dev-mode identities", async () => {
    const env = tokenEnv();
    for (const bearer of ["", "owner@personal", SECRET.slice(0, -1), `${SECRET}x`, SECRET.toUpperCase()]) {
      expect(await verifyToken(env, bearer)).toBeUndefined();
    }
  });

  it("fails closed when the secret or identity is unset", async () => {
    for (const unset of ["RELAY_TOKEN", "RELAY_USER_ID", "RELAY_ORG_ID"] as const) {
      expect(await verifyToken(tokenEnv({ [unset]: undefined }), SECRET)).toBeUndefined();
    }
    expect(await verifyToken(tokenEnv({ RELAY_TOKEN: "" }), "")).toBeUndefined();
  });

  it("accepts the secret as a header bearer or a WebSocket ?token=", async () => {
    const env = tokenEnv();
    const header = new Request("http://relay/registry/personal/ws", {
      headers: { authorization: `Bearer ${SECRET}` }
    });
    const query = new Request(`http://relay/chat2/c1/ws?token=${SECRET}`);
    expect(await authenticate(env, header)).toEqual({ userId: "owner", orgId: "personal" });
    expect(await authenticate(env, query)).toEqual({ userId: "owner", orgId: "personal" });
  });

  it("compares in constant time over equal-length digests", async () => {
    expect(await tokenMatches(SECRET, SECRET)).toBe(true);
    expect(await tokenMatches("a", "b")).toBe(false);
    expect(await tokenMatches("short", SECRET)).toBe(false);
  });
});
