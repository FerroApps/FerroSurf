(function () {
  "use strict";
  if (window.__ferroWebAuthn) return;
  if (!window.__ferrosurf || !window.__ferrosurf.call) return;
  if (!window.isSecureContext && location.hostname !== "localhost") return;
  if (typeof window.PublicKeyCredential === "function" && navigator.credentials && navigator.credentials.create) return;
  const bridge = window.__ferrosurf;
  Object.defineProperty(window, "__ferroWebAuthn", { value: true, enumerable: false });

  // ---- encoding helpers ----------------------------------------------------------------
  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
  function toBytes(src) {
    if (src instanceof ArrayBuffer) return new Uint8Array(src);
    if (ArrayBuffer.isView(src)) return new Uint8Array(src.buffer, src.byteOffset, src.byteLength);
    throw new TypeError("Expected a BufferSource");
  }
  function b64u(src) {
    const b = toBytes(src);
    let out = "";
    for (let i = 0; i < b.length; i += 3) {
      const n = (b[i] << 16) | ((b[i + 1] || 0) << 8) | (b[i + 2] || 0);
      out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63];
      if (i + 1 < b.length) out += B64[(n >> 6) & 63];
      if (i + 2 < b.length) out += B64[n & 63];
    }
    return out;
  }
  function fromB64u(s) {
    s = String(s).replace(/-/g, "+").replace(/_/g, "/").replace(/=+$/, "");
    const bin = atob(s + "===".slice((s.length + 3) % 4));
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out.buffer;
  }
  function utf8(s) { return new TextEncoder().encode(s); }
  function domError(name, message) {
    try { return new DOMException(message, name); } catch (e) { const x = new Error(message); x.name = name; return x; }
  }
  function mapError(e) {
    const m = /^(NotAllowedError|NotSupportedError|InvalidStateError|SecurityError|AbortError|ConstraintError|DataError|TypeError|UnknownError)(?::\s*(.*))?$/s.exec(String((e && e.message) || e));
    if (m) return m[1] === "TypeError" ? new TypeError(m[2] || "") : domError(m[1], m[2] || m[1]);
    return domError("NotAllowedError", "The operation either timed out or was not allowed.");
  }
  function defineClass(name, proto, ctor) {
    Object.defineProperty(ctor, "name", { value: name });
    Object.defineProperty(proto, Symbol.toStringTag, { value: name, configurable: true });
    return ctor;
  }
  const hidden = (obj, key, value) => Object.defineProperty(obj, key, { value, enumerable: false, writable: true, configurable: true });
  const idList = (list) => (list || []).map((d) => ({ id: b64u(d.id), type: d.type, transports: d.transports || [] }));

  // ---- response objects ----------------------------------------------------------------
  function AuthenticatorResponse() { throw new TypeError("Illegal constructor"); }
  Object.defineProperty(AuthenticatorResponse.prototype, "clientDataJSON", {
    get() { return this.__clientDataJSON; }, enumerable: true, configurable: true });
  defineClass("AuthenticatorResponse", AuthenticatorResponse.prototype, AuthenticatorResponse);

  function makeAttestationResponse(r, clientDataJSON) {
    const o = Object.create(AuthenticatorAttestationResponse.prototype);
    hidden(o, "__clientDataJSON", clientDataJSON);
    hidden(o, "__attestationObject", fromB64u(r.attestationObject));
    hidden(o, "__authData", fromB64u(r.authData));
    hidden(o, "__publicKey", fromB64u(r.publicKey));
    hidden(o, "__publicKeyAlg", r.publicKeyAlgorithm);
    hidden(o, "__transports", r.transports || ["internal"]);
    return o;
  }
  function AuthenticatorAttestationResponse() { throw new TypeError("Illegal constructor"); }
  AuthenticatorAttestationResponse.prototype = Object.create(AuthenticatorResponse.prototype, {
    constructor: { value: AuthenticatorAttestationResponse, writable: true, configurable: true } });
  Object.defineProperty(AuthenticatorAttestationResponse.prototype, "attestationObject", {
    get() { return this.__attestationObject; }, enumerable: true, configurable: true });
  Object.assign(AuthenticatorAttestationResponse.prototype, {
    getTransports() { return this.__transports.slice(); },
    getPublicKey() { return this.__publicKey; },
    getPublicKeyAlgorithm() { return this.__publicKeyAlg; },
    getAuthenticatorData() { return this.__authData; },
  });
  defineClass("AuthenticatorAttestationResponse", AuthenticatorAttestationResponse.prototype, AuthenticatorAttestationResponse);

  function makeAssertionResponse(r, clientDataJSON) {
    const o = Object.create(AuthenticatorAssertionResponse.prototype);
    hidden(o, "__clientDataJSON", clientDataJSON);
    hidden(o, "__authenticatorData", fromB64u(r.authenticatorData));
    hidden(o, "__signature", fromB64u(r.signature));
    hidden(o, "__userHandle", r.userHandle ? fromB64u(r.userHandle) : null);
    return o;
  }
  function AuthenticatorAssertionResponse() { throw new TypeError("Illegal constructor"); }
  AuthenticatorAssertionResponse.prototype = Object.create(AuthenticatorResponse.prototype, {
    constructor: { value: AuthenticatorAssertionResponse, writable: true, configurable: true } });
  for (const [k, f] of [["authenticatorData", "__authenticatorData"], ["signature", "__signature"], ["userHandle", "__userHandle"]]) {
    Object.defineProperty(AuthenticatorAssertionResponse.prototype, k, { get() { return this[f]; }, enumerable: true, configurable: true });
  }
  defineClass("AuthenticatorAssertionResponse", AuthenticatorAssertionResponse.prototype, AuthenticatorAssertionResponse);

  // ---- PublicKeyCredential -------------------------------------------------------------
  function Credential() { throw new TypeError("Illegal constructor"); }
  defineClass("Credential", Credential.prototype, Credential);
  function PublicKeyCredential() { throw new TypeError("Illegal constructor"); }
  PublicKeyCredential.prototype = Object.create(Credential.prototype, {
    constructor: { value: PublicKeyCredential, writable: true, configurable: true } });
  for (const k of ["id", "type", "rawId", "response", "authenticatorAttachment"]) {
    Object.defineProperty(PublicKeyCredential.prototype, k, { get() { return this["__" + k]; }, enumerable: true, configurable: true });
  }
  Object.assign(PublicKeyCredential.prototype, {
    getClientExtensionResults() { return {}; },
    toJSON() {
      const r = this.response, out = { clientDataJSON: b64u(r.clientDataJSON) };
      if (r instanceof AuthenticatorAttestationResponse) {
        out.authenticatorData = b64u(r.getAuthenticatorData());
        out.transports = r.getTransports();
        out.publicKey = b64u(r.getPublicKey());
        out.publicKeyAlgorithm = r.getPublicKeyAlgorithm();
        out.attestationObject = b64u(r.attestationObject);
      } else {
        out.authenticatorData = b64u(r.authenticatorData);
        out.signature = b64u(r.signature);
        if (r.userHandle) out.userHandle = b64u(r.userHandle);
      }
      return { id: this.id, rawId: b64u(this.rawId), type: this.type, authenticatorAttachment: this.authenticatorAttachment,
               clientExtensionResults: {}, response: out };
    },
  });
  function makeCredential(idB64, response) {
    const c = Object.create(PublicKeyCredential.prototype);
    hidden(c, "__id", idB64);
    hidden(c, "__type", "public-key");
    hidden(c, "__rawId", fromB64u(idB64));
    hidden(c, "__response", response);
    hidden(c, "__authenticatorAttachment", "platform");
    return c;
  }
  defineClass("PublicKeyCredential", PublicKeyCredential.prototype, PublicKeyCredential);
  Object.setPrototypeOf(PublicKeyCredential, Credential);
  const decodeDescs = (l) => (l || []).map((d) => Object.assign({}, d, { id: fromB64u(d.id) }));
  Object.assign(PublicKeyCredential, {
    isUserVerifyingPlatformAuthenticatorAvailable() { return Promise.resolve(true); },
    isConditionalMediationAvailable() { return Promise.resolve(false); },
    getClientCapabilities() {
      return Promise.resolve({ userVerifyingPlatformAuthenticator: true, conditionalCreate: false, conditionalGet: false,
        hybridTransport: false, passkeyPlatformAuthenticator: true, relatedOrigins: false, signalAllAcceptedCredentials: false });
    },
    parseCreationOptionsFromJSON(j) {
      return Object.assign({}, j, { challenge: fromB64u(j.challenge), user: Object.assign({}, j.user, { id: fromB64u(j.user.id) }),
        excludeCredentials: decodeDescs(j.excludeCredentials) });
    },
    parseRequestOptionsFromJSON(j) {
      return Object.assign({}, j, { challenge: fromB64u(j.challenge), allowCredentials: decodeDescs(j.allowCredentials) });
    },
  });

  // ---- navigator.credentials -----------------------------------------------------------
  function buildClientData(type, challenge) {
    return utf8(JSON.stringify({ type, challenge: b64u(challenge), origin: location.origin, crossOrigin: false }));
  }
  // The bridge is synchronous (it blocks inside prompt()), so run it from a task: the
  // promise is returned first and the page keeps a consistent execution order.
  function viaBridge(fn) {
    return new Promise((resolve, reject) => {
      setTimeout(() => { try { resolve(fn()); } catch (e) { reject(mapError(e)); } }, 0);
    });
  }
  function abortCheck(signal) {
    if (signal && signal.aborted) throw signal.reason || domError("AbortError", "The operation was aborted.");
  }

  function create(options) {
    options = options || {};
    if (!options.publicKey) return Promise.reject(domError("NotSupportedError", "Only publicKey credentials are supported."));
    try {
      abortCheck(options.signal);
      const pk = options.publicKey;
      if (!pk.rp || !pk.user || !pk.challenge) throw new TypeError("Invalid PublicKeyCredentialCreationOptions");
      const params = pk.pubKeyCredParams || [];
      if (params.length && !params.some((p) => p.type === "public-key" && p.alg === -7)) {
        throw domError("NotSupportedError", "No supported algorithm (only ES256).");
      }
      const clientData = buildClientData("webauthn.create", pk.challenge);
      const payload = {
        op: "create",
        clientDataJSON: b64u(clientData),
        rp: { id: pk.rp.id || location.hostname, name: pk.rp.name || "" },
        user: { id: b64u(pk.user.id), name: String(pk.user.name || ""), displayName: String(pk.user.displayName || "") },
        excludeCredentials: idList(pk.excludeCredentials),
        authenticatorSelection: pk.authenticatorSelection || {},
      };
      return viaBridge(() => {
        abortCheck(options.signal);
        const r = bridge.call("webauthn", payload);
        return makeCredential(r.credentialId, makeAttestationResponse(r, clientData.buffer));
      });
    } catch (e) { return Promise.reject(e); }
  }

  function get(options) {
    options = options || {};
    if (!options.publicKey) return Promise.reject(domError("NotSupportedError", "Only publicKey credentials are supported."));
    if (options.mediation === "conditional") return Promise.reject(domError("NotAllowedError", "Conditional mediation is not available."));
    try {
      abortCheck(options.signal);
      const pk = options.publicKey;
      if (!pk.challenge) throw new TypeError("Invalid PublicKeyCredentialRequestOptions");
      const clientData = buildClientData("webauthn.get", pk.challenge);
      const payload = {
        op: "get",
        clientDataJSON: b64u(clientData),
        rpId: pk.rpId || location.hostname,
        allowCredentials: idList(pk.allowCredentials),
        userVerification: pk.userVerification || "preferred",
      };
      return viaBridge(() => {
        abortCheck(options.signal);
        const r = bridge.call("webauthn", payload);
        return makeCredential(r.credentialId, makeAssertionResponse(r, clientData.buffer));
      });
    } catch (e) { return Promise.reject(e); }
  }

  const credentials = {
    create, get,
    store(c) { return Promise.resolve(c); },
    preventSilentAccess() { return Promise.resolve(); },
  };
  Object.defineProperty(credentials, Symbol.toStringTag, { value: "CredentialsContainer", configurable: true });

  const def = (obj, k, v) => Object.defineProperty(obj, k, { value: v, writable: true, configurable: true, enumerable: false });
  def(window, "PublicKeyCredential", PublicKeyCredential);
  def(window, "Credential", Credential);
  def(window, "AuthenticatorResponse", AuthenticatorResponse);
  def(window, "AuthenticatorAttestationResponse", AuthenticatorAttestationResponse);
  def(window, "AuthenticatorAssertionResponse", AuthenticatorAssertionResponse);
  if (navigator.credentials && typeof navigator.credentials === "object") {
    // Partial implementation (e.g. password credentials only): extend it.
    for (const k of ["create", "get"]) {
      const native = navigator.credentials[k] && navigator.credentials[k].bind(navigator.credentials);
      const ours = credentials[k];
      def(navigator.credentials, k, function (o) { return o && o.publicKey ? ours(o) : native ? native(o) : ours(o); });
    }
  } else {
    Object.defineProperty(navigator, "credentials", { value: credentials, configurable: true, enumerable: true });
  }
})();
