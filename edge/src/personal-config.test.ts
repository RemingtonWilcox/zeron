import { describe, expect, it } from "vitest";
import productionSource from "../wrangler.jsonc?raw";
import personalSource from "../wrangler.personal.jsonc?raw";

interface WranglerConfig {
  name: string;
  account_id?: string;
  routes?: unknown[];
  durable_objects: { bindings: { name: string; class_name: string }[] };
  migrations: { tag: string; new_sqlite_classes?: string[] }[];
  r2_buckets: { binding: string; bucket_name: string }[];
  vars: Record<string, string>;
}

/** JSONC → JSON: drop comments outside strings, then trailing commas. */
const parseJsonc = (source: string): WranglerConfig =>
  JSON.parse(
    source
      .replace(/("(?:\\.|[^"\\])*")|\/\/[^\n]*|\/\*[\s\S]*?\*\//g, (_, str) => str ?? "")
      .replace(/,(\s*[}\]])/g, "$1")
  ) as WranglerConfig;

const production = parseJsonc(productionSource);
const personal = parseJsonc(personalSource);

const createdClasses = (config: WranglerConfig) =>
  new Set(config.migrations.flatMap((m) => m.new_sqlite_classes ?? []));

// The personal relay (wrangler.personal.jsonc) deploys this same Worker to a
// different account. When the production config gains a binding or a Durable
// Object class, these fail until the personal config gets it too (a class
// also needs a new migration tag there).
describe("wrangler.personal.jsonc", () => {
  it("binds every Durable Object and R2 binding the production config binds", () => {
    expect(personal.durable_objects.bindings).toEqual(
      expect.arrayContaining(production.durable_objects.bindings)
    );
    expect(personal.r2_buckets.map((b) => b.binding).sort()).toEqual(
      production.r2_buckets.map((b) => b.binding).sort()
    );
  });

  it("creates every bound Durable Object class in its migrations", () => {
    const created = createdClasses(personal);
    for (const { class_name } of personal.durable_objects.bindings) {
      expect(created, class_name).toContain(class_name);
    }
  });

  it("shares no worker, bucket, route, or account with production", () => {
    expect(personal.name).not.toBe(production.name);
    expect(personal.account_id).toBeUndefined();
    expect(personal.routes ?? []).toEqual([]);
    const productionBuckets = new Set(production.r2_buckets.map((b) => b.bucket_name));
    for (const { bucket_name } of personal.r2_buckets) {
      expect(productionBuckets).not.toContain(bucket_name);
    }
    // /releases/* is unauthenticated, so RELEASES must never alias BLOBS.
    const bucket = (binding: string) =>
      personal.r2_buckets.find((b) => b.binding === binding)?.bucket_name;
    expect(bucket("RELEASES")).not.toBe(bucket("BLOBS"));
  });

  it("verifies real WorkOS tokens", () => {
    expect(personal.vars.AUTH_MODE).toBe("workos");
    expect(personal.vars.WORKOS_CLIENT_ID).not.toBe(production.vars.WORKOS_CLIENT_ID);
  });
});
