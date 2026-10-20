// Behavioral spec for the shared client runtime (src/runtime.ts).
//
// This executes the real executor against an injected fetch — no
// generation involved. Descriptor interpretation, transport policy,
// codecs, and the bounded replay contract run as code.
//
// The runtime holds no credential state: it attaches what `getToken`
// returns, guards the unsafe combinations, and after a 401 re-sends
// through `onUnauthorized` at most twice. Everything about *when* a
// session renews is the application's, and the tests below pin the line
// between the two.

import { describe, test, expect, afterEach } from "bun:test";
import {
  ApiError,
  createTransport,
  createTypedWebSocket,
  jwtExpiry,
  singleFlight,
} from "../../src/runtime.ts";

// bun's runner hangs on a deferred `expect(p).rejects` across intervening
// awaits — capture the message via an immediately-attached handler instead.
const rejectionOf = (p) => {
  let message = null;
  let settled = false;
  p.then(
    () => {
      settled = true;
    },
    (e) => {
      settled = true;
      message = e?.message ?? String(e);
    },
  );
  return () => {
    if (!settled) throw new Error("promise not settled (hang guard)");
    return message;
  };
};

const jsonResponse = (body, status = 200) =>
  new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });

const always401 = () => jsonResponse({ error: "expired" }, 401);

// Sequential fetch mock: each step is (url, init) => Response (or a promise
// of one). Every call is recorded in the shared log; the last step repeats.
const seqFetch = (steps, log) => (url, init) => {
  log.push({ url, init });
  const step = steps[Math.min(log.length - 1, steps.length - 1)];
  return Promise.resolve(step(url, init));
};

const OPS = {
  me: { method: "GET", path: "/auth/me", auth: "none", response: "json" },
  privateThing: {
    method: "GET",
    path: "/private",
    auth: "required",
    response: "json",
  },
  feed: { method: "GET", path: "/feed", auth: "optional", response: "json" },
};

const opts = (fetch, extra = {}) => ({
  baseUrl: "https://api.test",
  credentials: "same-origin",
  fetch,
  ...extra,
});

const POLICY = { scheme: "bearer" };

const b64url = (obj) =>
  btoa(JSON.stringify(obj)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const jwtWithExp = (expSec) => `${b64url({ alg: "none" })}.${b64url({ exp: expSec })}.sig`;

const bearerOf = (log) => log.map((e) => e.init?.headers?.Authorization ?? null);

// ---------------------------------------------------------------------------
// The replay contract
// ---------------------------------------------------------------------------

describe("replay: the application's renewal policy", () => {
  test("the replay carries the hook's return value", async () => {
    const log = [];
    const t = createTransport(
      opts(seqFetch([always401, () => jsonResponse({ ok: true })], log), {
        getToken: () => "A",
        onUnauthorized: async () => "A2",
      }),
      POLICY,
      OPS,
    );
    expect(await t.execute(OPS.privateThing)).toEqual({ ok: true });
    expect(bearerOf(log)).toEqual(["Bearer A", "Bearer A2"]);
  });

  test("replay is bounded: three sends, two hook calls, then terminal", async () => {
    const log = [];
    let renewals = 0;
    const t = createTransport(
      opts(seqFetch([always401], log), {
        getToken: () => "A",
        onUnauthorized: async () => {
          renewals += 1;
          return `A${renewals}`;
        },
      }),
      POLICY,
      OPS,
    );
    const msg = rejectionOf(t.execute(OPS.privateThing));
    await new Promise((r) => setTimeout(r, 20));
    expect(msg()).toMatch(/expired/);
    expect(log.length).toBe(3);
    expect(renewals).toBe(2);
    expect(bearerOf(log)).toEqual(["Bearer A", "Bearer A1", "Bearer A2"]);
  });

  test("a hook returning null surfaces the server's own 401", async () => {
    const log = [];
    const t = createTransport(
      opts(seqFetch([always401], log), {
        getToken: () => "A",
        onUnauthorized: async () => null,
      }),
      POLICY,
      OPS,
    );
    const msg = rejectionOf(t.execute(OPS.privateThing));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/expired/);
    expect(log.length).toBe(1);
  });

  test("a throwing hook surfaces the server's 401, not the hook's error", async () => {
    // The renewal failure belongs to the application; the request's real
    // failure is the 401. A rejected request must not carry a hook error
    // the caller cannot act on.
    const log = [];
    const t = createTransport(
      opts(seqFetch([always401], log), {
        getToken: () => "A",
        onUnauthorized: async () => {
          throw new Error("refresh endpoint unreachable");
        },
      }),
      POLICY,
      OPS,
    );
    const msg = rejectionOf(t.execute(OPS.privateThing));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/^expired/);
    expect(log.length).toBe(1);
  });

  test("a hook returning the rejected credential does not re-send", async () => {
    const log = [];
    let calls = 0;
    const t = createTransport(
      opts(seqFetch([always401], log), {
        getToken: () => "stale",
        onUnauthorized: async () => {
          calls += 1;
          return "stale";
        },
      }),
      POLICY,
      OPS,
    );
    const msg = rejectionOf(t.execute(OPS.privateThing));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/expired/);
    expect(log.length).toBe(1);
    expect(calls).toBe(1);
  });

  test("a declined renewal never re-sends under a credential the app switched to", async () => {
    // The request goes out under account A. By the time its 401 lands the
    // app has switched to B and cannot renew. The transport must not
    // substitute B just because `getToken` would now hand it over: a
    // request never changes principal.
    const log = [];
    let current = "A";
    const t = createTransport(
      opts(seqFetch([always401], log), {
        getToken: () => current,
        onUnauthorized: async () => {
          current = "B";
          return null;
        },
      }),
      POLICY,
      OPS,
    );
    const msg = rejectionOf(t.execute(OPS.privateThing));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/expired/);
    expect(bearerOf(log)).toEqual(["Bearer A"]);
  });

  test("optional routes do not replay", async () => {
    const log = [];
    let calls = 0;
    const t = createTransport(
      opts(seqFetch([always401], log), {
        getToken: () => "A",
        onUnauthorized: async () => {
          calls += 1;
          return "A2";
        },
      }),
      POLICY,
      OPS,
    );
    const msg = rejectionOf(t.execute(OPS.feed));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/expired/);
    expect(calls).toBe(0);
    expect(log.length).toBe(1);
  });

  test("cookie routes do not replay — nothing can change the jar mid-flight", async () => {
    const log = [];
    let calls = 0;
    const t = createTransport(
      opts(seqFetch([always401], log), {
        onUnauthorized: async () => {
          calls += 1;
          return "A2";
        },
      }),
      { scheme: "cookie" },
      OPS,
    );
    const msg = rejectionOf(t.execute(OPS.privateThing));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/expired/);
    expect(calls).toBe(0);
    expect(log.length).toBe(1);
  });

  test("without a hook, a 401 is terminal on the first send", async () => {
    const log = [];
    const t = createTransport(
      opts(seqFetch([always401], log), { getToken: () => "A" }),
      POLICY,
      OPS,
    );
    const msg = rejectionOf(t.execute(OPS.privateThing));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/expired/);
    expect(log.length).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// The bearer source
// ---------------------------------------------------------------------------

describe("bearer source", () => {
  test("getToken is consulted per call, so a renewed value is picked up", async () => {
    const log = [];
    let current = "first";
    const t = createTransport(
      opts(seqFetch([() => jsonResponse({ ok: true })], log), { getToken: () => current }),
      POLICY,
      OPS,
    );
    await t.execute(OPS.privateThing);
    current = "second";
    await t.execute(OPS.privateThing);
    expect(bearerOf(log)).toEqual(["Bearer first", "Bearer second"]);
  });

  test("a sync getToken is accepted alongside an async one", async () => {
    const log = [];
    const t = createTransport(
      opts(seqFetch([() => jsonResponse({ ok: true })], log), { getToken: () => "sync-tok" }),
      POLICY,
      OPS,
    );
    await t.execute(OPS.privateThing);
    expect(bearerOf(log)).toEqual(["Bearer sync-tok"]);
  });

  test("a required route with no token source fails loudly", async () => {
    const log = [];
    const t = createTransport(opts(seqFetch([() => jsonResponse({ ok: true })], log)), POLICY, OPS);
    const msg = rejectionOf(t.execute(OPS.privateThing));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/no token source was configured/);
    expect(log.length).toBe(0);
  });

  test("a required route with an empty source refuses to send unauthenticated", async () => {
    const log = [];
    const t = createTransport(
      opts(seqFetch([() => jsonResponse({ ok: true })], log), { getToken: () => null }),
      POLICY,
      OPS,
    );
    const msg = rejectionOf(t.execute(OPS.privateThing));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/returned no token/);
    expect(log.length).toBe(0);
  });

  test("an optional route rides a credential when one exists and skips when not", async () => {
    const log = [];
    const withTok = createTransport(
      opts(seqFetch([() => jsonResponse({ ok: true })], log), { getToken: () => "A" }),
      POLICY,
      OPS,
    );
    await withTok.execute(OPS.feed);
    const without = createTransport(
      opts(seqFetch([() => jsonResponse({ ok: true })], log)),
      POLICY,
      OPS,
    );
    await without.execute(OPS.feed);
    expect(bearerOf(log)).toEqual(["Bearer A", null]);
  });
});

// ---------------------------------------------------------------------------
// Application-facing primitives
// ---------------------------------------------------------------------------

describe("primitives: jwtExpiry and singleFlight", () => {
  test("jwtExpiry reads the exp claim and degrades to null", () => {
    const future = Math.floor(Date.now() / 1000) + 3600;
    expect(jwtExpiry(jwtWithExp(future))).toBe(future * 1000);
    expect(jwtExpiry("not-a-jwt")).toBeNull();
    expect(jwtExpiry(`${b64url({})}.${b64url({ sub: 1 })}.sig`)).toBeNull();
  });

  test("singleFlight coalesces concurrent callers into one call", async () => {
    let calls = 0;
    const renew = singleFlight(async () => {
      calls += 1;
      await new Promise((r) => setTimeout(r, 10));
      return `t${calls}`;
    });
    const joined = await Promise.all([renew(), renew(), renew()]);
    expect(calls).toBe(1);
    expect(joined).toEqual(["t1", "t1", "t1"]);
    // The next caller after settling starts fresh — the bound is per
    // in-flight call, not per lifetime.
    expect(await renew()).toBe("t2");
    expect(calls).toBe(2);
  });

  test("singleFlight releases the slot when the call rejects", async () => {
    let calls = 0;
    const renew = singleFlight(async () => {
      calls += 1;
      throw new Error("network down");
    });
    await renew().catch(() => {});
    await renew().catch(() => {});
    expect(calls).toBe(2);
  });
});

// ---------------------------------------------------------------------------
// Descriptors drive transport policy
// ---------------------------------------------------------------------------

describe("executor: descriptors drive transport policy", () => {
  test("path params are interpolated and encoded; missing ones fail loudly", async () => {
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ ok: 1 })], log);
    const t = createTransport(opts(fetch), POLICY, {});
    await t.execute(
      { method: "GET", path: "/users/{id}", params: ["id"], auth: "none", response: "json" },
      { params: { id: "a b/c" } },
    );
    expect(log[0].url).toBe("https://api.test/users/a%20b%2Fc");

    const msg = rejectionOf(
      t.execute(
        { method: "GET", path: "/users/{id}", params: ["id"], auth: "none", response: "json" },
        {},
      ),
    );
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/Missing path parameter "id"/);
  });

  test("Content-Type rides only body-carrying calls; auth'd calls skip the cache", async () => {
    const log = [];
    const fetch = seqFetch(
      [() => jsonResponse({ ok: 1 }), () => jsonResponse({ ok: 2 }), () => jsonResponse({ ok: 3 })],
      log,
    );
    const ops = {
      post: { method: "POST", path: "/p", auth: "none", body: "json", response: "json" },
      pub: { method: "GET", path: "/pub", auth: "none", response: "json" },
      priv: { method: "GET", path: "/priv", auth: "required", response: "json" },
    };
    const t = createTransport(opts(fetch, { getToken: () => "tok" }), POLICY, ops);

    await t.execute(ops.post, { body: { a: 1 } });
    await t.execute(ops.pub);
    await t.execute(ops.priv);

    expect(log[0].init.headers["Content-Type"]).toBe("application/json");
    expect(log[1].init.headers["Content-Type"]).toBeUndefined();
    expect(log[2].init.headers["Content-Type"]).toBeUndefined();
    expect(log[0].init.cache).toBeUndefined();
    expect(log[1].init.cache).toBeUndefined();
    expect(log[2].init.cache).toBe("no-store");
  });

  test("only credentialless allow_redirects routes follow redirects", async () => {
    const log = [];
    const fetch = seqFetch(
      [() => jsonResponse({ ok: 1 }), () => jsonResponse({ ok: 2 }), () => jsonResponse({ ok: 3 })],
      log,
    );
    const allow = { method: "GET", path: "/r", auth: "none", allowRedirects: true, response: "json" };
    const priv = { method: "GET", path: "/priv", auth: "required", response: "json" };

    const tOmit = createTransport(opts(fetch, { credentials: "omit" }), POLICY, {});
    await tOmit.execute(allow);
    expect(log[0].init.redirect).toBe("follow");

    const tSame = createTransport(opts(fetch), POLICY, {});
    await tSame.execute(allow);
    expect(log[1].init.redirect).toBe("error");

    const tAuth = createTransport(opts(fetch, { getToken: () => "tok" }), POLICY, { priv });
    await tAuth.execute(priv);
    expect(log[2].init.redirect).toBe("error");
  });

  test("empty success bodies resolve to undefined (204-safe)", async () => {
    const log = [];
    const fetch = seqFetch(
      [() => new Response(null, { status: 204 }), () => new Response("", { status: 200 })],
      log,
    );
    const t = createTransport(opts(fetch), POLICY, {});
    const a = await t.execute({ method: "POST", path: "/x", auth: "none", response: "void" });
    const b = await t.execute({ method: "GET", path: "/y", auth: "none", response: "json" });
    expect(a).toBeUndefined();
    expect(b).toBeUndefined();
  });

  test("error surface: message from JSON body, fallback to text, typed ApiError", async () => {
    const log = [];
    const fetch = seqFetch(
      [() => jsonResponse({ error: "boom" }, 500), () => new Response("plain nope", { status: 502 })],
      log,
    );
    const t = createTransport(opts(fetch), POLICY, {});
    const op = { method: "GET", path: "/x", auth: "none", response: "json" };
    const msg1 = rejectionOf(t.execute(op));
    const msg2 = rejectionOf(t.execute(op));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg1()).toMatch(/^boom/);
    expect(msg2()).toMatch(/plain nope/);

    // The typed error still carries status and parsed body.
    let caught = null;
    try {
      await t.execute(op);
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeInstanceOf(ApiError);
    expect(caught.status).toBe(502);
  });

  test("an unknown OPERATIONS key fails loudly instead of dereferencing undefined", async () => {
    const log = [];
    const t = createTransport(opts(seqFetch([() => jsonResponse({ ok: true })], log)), POLICY, OPS);
    const msg = rejectionOf(t.execute("noSuchKey"));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/Unknown operation "noSuchKey"/);
    expect(log.length).toBe(0);
  });

  test("query arrays expand to repeated keys; nullish values drop", async () => {
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ ok: 1 })], log);
    const t = createTransport(opts(fetch), POLICY, {});
    await t.execute(
      { method: "GET", path: "/q", auth: "none", response: "json" },
      { query: { tag: ["a", "b"], skip: null, take: 10 } },
    );
    expect(log[0].url).toBe("https://api.test/q?tag=a&tag=b&take=10");
  });
});

describe("executor: schemes and guards", () => {
  test("cookie scheme: omit refuses required routes, permits optional ones", async () => {
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ ok: 1 })], log);
    const t = createTransport(opts(fetch, { credentials: "omit" }), { scheme: "cookie" }, {});

    const msg = rejectionOf(t.execute(OPS.privateThing));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/refusing to send an unauthenticated request/);

    await t.execute(OPS.feed);
    expect(log.length).toBe(1);
  });

  test("cookie scheme: CSRF proof accepts sync callbacks too", async () => {
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ ok: 1 }), () => jsonResponse({ ok: 2 })], log);
    const mutate = { method: "POST", path: "/mutate", auth: "required", body: "json", response: "json" };
    const t = createTransport(
      opts(fetch, { csrfToken: () => "sync-proof", csrfHeader: "X-CSRF-Token" }),
      { scheme: "cookie" },
      {},
    );
    await t.execute(mutate, { body: {} });
    // null-returning sync callbacks attach nothing.
    const t2 = createTransport(
      opts(fetch, { csrfToken: () => null, csrfHeader: "X-CSRF-Token" }),
      { scheme: "cookie" },
      {},
    );
    await t2.execute(mutate, { body: {} });
    expect(log[0].init.headers["X-CSRF-Token"]).toBe("sync-proof");
    expect(log[1].init.headers["X-CSRF-Token"]).toBeUndefined();
  });

  test("cookie scheme: CSRF proof rides mutating calls only, under the configured header", async () => {
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ ok: 1 }), () => jsonResponse({ ok: 2 })], log);
    const t = createTransport(
      opts(fetch, { csrfToken: async () => "proof", csrfHeader: "X-CSRF-Token" }),
      { scheme: "cookie" },
      {},
    );
    await t.execute(
      { method: "POST", path: "/mutate", auth: "required", body: "json", response: "json" },
      { body: {} },
    );
    await t.execute({ method: "GET", path: "/read", auth: "required", response: "json" });
    expect(log[0].init.headers["X-CSRF-Token"]).toBe("proof");
    expect(log[1].init.headers["X-CSRF-Token"]).toBeUndefined();
  });

  test("transport guard: credentialed http to non-loopback refuses; loopback passes", async () => {
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ ok: 1 })], log);
    const lan = createTransport(opts(fetch, { baseUrl: "http://192.168.1.20:8080" }), POLICY, {});
    const msg = rejectionOf(lan.execute(OPS.feed)); // even optional: cookies may ride
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/Refusing to send credentials over http/);

    const loopback = createTransport(
      opts(fetch, { baseUrl: "http://127.0.0.1:8080" }),
      POLICY,
      {},
    );
    const out = await loopback.execute(OPS.feed);
    expect(out).toEqual({ ok: 1 });
  });
});

describe("transport: WebSocket", () => {
  const origWS = globalThis.WebSocket;
  let wsUrls;
  afterEach(() => {
    globalThis.WebSocket = origWS;
  });

  const stubWS = () => {
    wsUrls = [];
    globalThis.WebSocket = class {
      constructor(url) {
        wsUrls.push(url);
      }
    };
  };

  test("ticket handshake speaks the ticket op's method, credentialed, and pins the proof to the URL", async () => {
    stubWS();
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ ticket: "tkt" })], log);
    const ops = {
      voice: { method: "GET", path: "/voice", auth: "required", response: "void", ws: true },
      ticket: { method: "POST", path: "/ws/ticket", auth: "required", response: "json" },
    };
    const t = createTransport(
      opts(fetch, { getToken: () => "tok" }),
      { scheme: "bearer", wsTicket: "ticket" },
      ops,
    );
    await t.connect(ops.voice);
    expect(log[0].init.method).toBe("POST");
    expect(log[0].url).toBe("https://api.test/ws/ticket");
    expect(log[0].init.headers.Authorization).toBe("Bearer tok");
    expect(wsUrls[0]).toBe("wss://api.test/voice?ticket=tkt");
  });

  test("a config-only client wires the ticket handshake from getToken alone", async () => {
    stubWS();
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ ticket: "cfg" })], log);
    const ops = {
      voice: { method: "GET", path: "/voice", auth: "required", response: "void", ws: true },
      issueTicket: { method: "GET", path: "/ws/ticket", auth: "required", response: "json" },
    };
    const t = createTransport(
      opts(fetch, { getToken: async () => "static-tok" }),
      { scheme: "bearer", wsTicket: "issueTicket" },
      ops,
    );
    await t.connect(ops.voice);
    expect(log[0].init.method).toBe("GET");
    expect(log[0].url).toBe("https://api.test/ws/ticket");
    expect(log[0].init.headers.Authorization).toBe("Bearer static-tok");
    expect(wsUrls[0]).toBe("wss://api.test/voice?ticket=cfg");
  });

  test("ws query arrays expand to repeated keys like HTTP calls", async () => {
    stubWS();
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ ok: 1 })], log);
    const t = createTransport(opts(fetch), POLICY, {});
    await t.connect(
      { method: "GET", path: "/voice", auth: "none", response: "void", ws: true },
      { query: { tag: ["a", "b"], room: "lobby" } },
    );
    expect(wsUrls[0]).toBe("wss://api.test/voice?tag=a&tag=b&room=lobby");
  });

  test("a cookie ws upgrade under credentials: omit is refused, not sent anonymously", async () => {
    stubWS();
    const log = [];
    const fetch = seqFetch([() => jsonResponse({ ok: 1 })], log);
    const t = createTransport(
      opts(fetch, { credentials: "omit" }),
      { scheme: "cookie" },
      {},
    );
    let msg = null;
    try {
      await t.connect({ method: "GET", path: "/voice", auth: "required", response: "void", ws: true });
    } catch (e) {
      msg = e.message;
    }
    expect(msg).toMatch(/refusing to send an unauthenticated upgrade/);
    expect(wsUrls.length).toBe(0);
  });

  // Minimal event-target socket stub: collects addEventListener
  // subscriptions so tests can emit and assert multi-subscriber delivery.
  const stubSocket = () => {
    const listeners = {};
    const stub = {
      listeners,
      sent: [],
      closes: [],
      addEventListener: (type, fn) => {
        (listeners[type] ??= []).push(fn);
      },
      emit: (type, ev) => (listeners[type] ?? []).forEach((fn) => fn(ev)),
      send: (d) => stub.sent.push(d),
      close: (...args) => stub.closes.push(args),
    };
    return stub;
  };

  test("typed sockets speak JSON both ways", () => {
    const stub = stubSocket();
    const socket = createTypedWebSocket(stub);
    let received;
    socket.onMessage((e) => (received = e));
    socket.send({ a: 1 });
    stub.emit("message", { data: '{"b":2}' });
    expect(stub.sent[0]).toBe('{"a":1}');
    expect(received).toEqual({ b: 2 });
  });

  test("socket subscribers accumulate instead of replacing each other", () => {
    const stub = stubSocket();
    const socket = createTypedWebSocket(stub);
    const got = [];
    socket.onMessage((e) => got.push(["first", e]));
    socket.onMessage((e) => got.push(["second", e]));
    stub.emit("message", { data: '{"n":1}' });
    expect(got).toEqual([
      ["first", { n: 1 }],
      ["second", { n: 1 }],
    ]);
    const opened = [];
    socket.onOpen(() => opened.push(1));
    socket.onOpen(() => opened.push(2));
    stub.emit("open", {});
    expect(opened).toEqual([1, 2]);
  });

  test("close forwards application code and reason", () => {
    const stub = stubSocket();
    const socket = createTypedWebSocket(stub);
    socket.close(4000, "session replaced");
    socket.close(1000);
    socket.close();
    expect(stub.closes).toEqual([
      [4000, "session replaced"],
      [1000],
      [],
    ]);
  });
});

describe("executor: observers cannot consume the transport's body", () => {
  test("onResponse gets a clone — its own body read leaves the typed result intact", async () => {
    const log = [];
    let observed;
    const t = createTransport(
      opts(seqFetch([() => jsonResponse({ value: 42 })], log), {
        onResponse: async (_op, response) => {
          observed = await response.json(); // reads the observer's copy
        },
      }),
      POLICY,
      OPS,
    );
    const result = await t.execute(OPS.feed);
    expect(observed).toEqual({ value: 42 });
    expect(result).toEqual({ value: 42 });
  });

  test("a status-only observer does not hang a successful void call", async () => {
    const log = [];
    let observedStatus;
    const t = createTransport(
      opts(seqFetch([() => new Response("ok")], log), {
        // Deliberately never reads or cancels the clone: the void path must
        // not wait for the observer's copy before resolving.
        onResponse: (_op, response) => {
          observedStatus = response.status;
        },
      }),
      POLICY,
      OPS,
    );
    let timer;
    const result = await Promise.race([
      t.execute({ method: "POST", path: "/x", auth: "none", response: "void" }),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error("void call hung")), 1000);
      }),
    ]);
    clearTimeout(timer);
    expect(result).toBeUndefined();
    expect(observedStatus).toBe(200);
  });

  test("void resolution never depends on a teed body's cancel settling", async () => {
    // Node (undici) keeps a branch cancel pending while its clone is
    // unread, unlike bun, so a real Response cannot pin this invariant on
    // every runner. Model the worst case: cancel never settles.
    const response = {
      ok: true,
      status: 200,
      clone: () => ({ ok: true, status: 200 }),
      body: { cancel: () => new Promise(() => {}) },
      arrayBuffer: async () => new ArrayBuffer(0),
    };
    const t = createTransport(
      opts(() => response, { onResponse: () => {} }),
      POLICY,
      OPS,
    );
    let timer;
    const result = await Promise.race([
      t.execute({ method: "POST", path: "/x", auth: "none", response: "void" }),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error("void call hung")), 1000);
      }),
    ]);
    clearTimeout(timer);
    expect(result).toBeUndefined();
  });

  test("a throwing observer never breaks the call", async () => {
    const boom = () => {
      throw new Error("telemetry bug");
    };
    const ok = createTransport(
      opts(seqFetch([() => jsonResponse({ fine: true })], []), {
        onRequest: boom,
        onResponse: boom,
      }),
      POLICY,
      OPS,
    );
    expect(await ok.execute(OPS.feed)).toEqual({ fine: true });

    // The error path too: a throwing onError must not mask the ApiError.
    const failing = createTransport(
      opts(seqFetch([() => jsonResponse({ error: "nope" }, 500)], []), { onError: boom }),
      POLICY,
      OPS,
    );
    const msg = rejectionOf(failing.execute(OPS.feed));
    await new Promise((r) => setTimeout(r, 10));
    expect(msg()).toMatch(/nope/);
  });
});