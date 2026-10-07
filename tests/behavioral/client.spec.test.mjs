// Integration spec: the REAL generated client (both files, emitted by
// generate_to_file into $AXOTYPED_CLIENT_DIR) runs end-to-end under an
// injected fetch. Proves the generator's wiring — descriptors, declaration
// as data, factory, auth namespace, renewal helper — not just the runtime
// in isolation.

import { describe, test, expect } from "bun:test";

const dir = process.env.AXOTYPED_CLIENT_DIR;
if (!dir) throw new Error("AXOTYPED_CLIENT_DIR not set (run via cargo test --test behavioral)");

const mod = await import(Bun.pathToFileURL(`${dir}/index.ts`).href);
const createClient = mod.createApiClient;

const jsonResponse = (body, status = 200) =>
  new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });

const seqFetch = (steps, log) => (url, init) => {
  log.push({ url, init });
  const step = steps[Math.min(log.length - 1, steps.length - 1)];
  return Promise.resolve(step(url, init));
};

const opts = (fetch, extra = {}) => ({
  baseUrl: "https://api.test",
  credentials: "same-origin",
  fetch,
  ...extra,
});

// The application owns the credential. This is the whole session layer the
// library used to keep: where it lives, when it renews, what a lapse means.
const sessionFixture = (initial = "old") => {
  let token = initial;
  let renewals = 0;
  const store = {
    read: () => token,
    write: (t) => {
      token = t;
    },
  };
  return { store, renewals: () => renewals };
};

describe("generated client", () => {
  test("imports its runtime and asserts the spec version (module loaded)", () => {
    expect(typeof createClient).toBe("function");
    expect(typeof mod.OPERATIONS).toBe("object");
    // The declaration lands as data the app reads — nothing in the runtime
    // interprets it.
    expect(mod.AUTH.kind).toBe("exchange");
    expect(mod.AUTH.refresh).toBe("me");
    expect(mod.AUTH.field).toBe("token");
    expect(mod.AUTH.bootstrap).toEqual(["me"]);
  });

  test("the declaration buys a typed renewal helper", () => {
    expect(typeof mod.refreshBearer).toBe("function");
  });

  test("the synthetic auth namespace aliases the lifecycle route", async () => {
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ authenticated: true, token: "t1" })], log);
    const client = createClient(opts(fetch));
    const me = await client.auth.me();
    expect(me.authenticated).toBe(true);
    expect(log[0].url).toBe("https://api.test/auth/me");
  });

  test("refreshBearer lifts the declared field off the lifecycle response", async () => {
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ authenticated: true, token: "v2" })], log);
    const client = createClient(opts(fetch));
    expect(await mod.refreshBearer(client)).toBe("v2");
    expect(log[0].url).toBe("https://api.test/auth/me");
  });

  test("refreshBearer returns null when the response carries no token", async () => {
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ authenticated: false, token: null })], log);
    const client = createClient(opts(fetch));
    expect(await mod.refreshBearer(client)).toBeNull();
  });

  test("the generated helper pairs with getToken and onUnauthorized for 401 replay", async () => {
    const log = [];
    const { store } = sessionFixture("old");
    const fetch = seqFetch(
      [
        () => jsonResponse({ error: "no" }, 401),
        () => jsonResponse({ authenticated: true, token: "v2" }),
        () => jsonResponse({ value: 42 }),
      ],
      log,
    );
    // The application's whole session layer: read the credential, renew on
    // a 401 through the generated helper, persist what came back.
    const client = createClient(
      opts(fetch, {
        getToken: () => store.read(),
        onUnauthorized: async () => {
          const fresh = await mod.refreshBearer(client);
          if (fresh) store.write(fresh);
          return fresh;
        },
      }),
    );
    // private_thing declares no response type — void contract.
    await client.privateThing();
    expect(log[0].init.headers.Authorization).toBe("Bearer old");
    expect(log[2].init.headers.Authorization).toBe("Bearer v2");
    expect(store.read()).toBe("v2");
  });

  test("withOptions derives a working client over the same credential source", async () => {
    const log = [];
    const { store } = sessionFixture("tok");
    const fetch = seqFetch([() => jsonResponse({ value: 7 })], log);
    const client = createClient(opts(fetch, { getToken: () => store.read() }));
    const derived = client.withOptions({ baseUrl: "https://api.test" });
    await derived.privateThing();
    expect(log[0].init.headers.Authorization).toBe("Bearer tok");
  });
});