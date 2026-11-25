// https://w3c.github.io/webdriver-bidi/#protocol

use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(crate) struct BidiError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl BidiError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new("invalid argument", message)
    }

    pub(crate) fn no_such_frame(message: impl Into<String>) -> Self {
        Self::new("no such frame", message)
    }

    pub(crate) fn no_such_node(message: impl Into<String>) -> Self {
        Self::new("no such node", message)
    }

    pub(crate) fn no_such_element(message: impl Into<String>) -> Self {
        Self::new("no such element", message)
    }

    pub(crate) fn move_target_out_of_bounds(message: impl Into<String>) -> Self {
        Self::new("move target out of bounds", message)
    }

    pub(crate) fn no_such_handle(message: impl Into<String>) -> Self {
        Self::new("no such handle", message)
    }

    pub(crate) fn no_such_script(message: impl Into<String>) -> Self {
        Self::new("no such script", message)
    }

    pub(crate) fn no_such_intercept(message: impl Into<String>) -> Self {
        Self::new("no such intercept", message)
    }

    pub(crate) fn no_such_request(message: impl Into<String>) -> Self {
        Self::new("no such request", message)
    }

    pub(crate) fn no_such_user_context(message: impl Into<String>) -> Self {
        Self::new("no such user context", message)
    }

    pub(crate) fn no_such_alert(message: impl Into<String>) -> Self {
        Self::new("no such alert", message)
    }

    pub(crate) fn unable_to_capture_screen(message: impl Into<String>) -> Self {
        Self::new("unable to capture screen", message)
    }

    pub(crate) fn unable_to_set_cookie(message: impl Into<String>) -> Self {
        Self::new("unable to set cookie", message)
    }

    pub(crate) fn unknown_command(message: impl Into<String>) -> Self {
        Self::new("unknown command", message)
    }

    pub(crate) fn unknown_error(message: impl Into<String>) -> Self {
        Self::new("unknown error", message)
    }

    pub(crate) fn session_not_created(message: impl Into<String>) -> Self {
        Self::new("session not created", message)
    }
}

pub(crate) type BidiResult = Result<Value, BidiError>;

#[must_use]
pub(crate) fn ok_response(id: u64, result: &Value) -> Value {
    json!({
        "type": "success",
        "id": id,
        "result": result,
    })
}

#[must_use]
pub(crate) fn error_response(id: Option<u64>, err: &BidiError) -> Value {
    json!({
        "type": "error",
        "id": id,
        "error": err.code,
        "message": err.message,
    })
}

#[must_use]
pub(crate) fn event_message(method: &str, params: &Value) -> Value {
    json!({
        "type": "event",
        "method": method,
        "params": params,
    })
}

pub(crate) fn require_object<'a>(
    value: &'a Value,
    name: &str,
) -> Result<&'a serde_json::Map<String, Value>, BidiError> {
    value
        .as_object()
        .ok_or_else(|| BidiError::invalid_argument(format!("{name} must be an object")))
}

pub(crate) fn require_str<'a>(
    map: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a str, BidiError> {
    map.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| BidiError::invalid_argument(format!("{key} must be a string")))
}

pub(crate) fn optional_str<'a>(
    map: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, BidiError> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(BidiError::invalid_argument(format!(
            "{key} must be a string"
        ))),
    }
}

pub(crate) fn optional_bool(
    map: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<bool>, BidiError> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(BidiError::invalid_argument(format!(
            "{key} must be a boolean"
        ))),
    }
}

// https://w3c.github.io/webdriver-bidi/#type-script-RemoteValue
// https://w3c.github.io/webdriver-bidi/#type-script-LocalValue
pub(crate) const BIDI_RUNTIME_JS: &str = r"
const __tbBidi = (() => {
  if (!host.__tbBidiState) {
    host.__tbBidiState = {
      nextHandle: 1,
      handles: Object.create(null),
      sharedNodes: new WeakMap(),
      sharedById: Object.create(null),
      nextShared: 1,
    };
  }
  const st = host.__tbBidiState;

  function allocHandle(value) {
    const id = 'h-' + (st.nextHandle++);
    st.handles[id] = value;
    return id;
  }

  function sharedIdFor(node, contextId) {
    let existing = st.sharedNodes.get(node);
    if (existing) return existing;
    const id = contextId + '_n_' + (st.nextShared++);
    st.sharedNodes.set(node, id);
    st.sharedById[id] = node;
    return id;
  }

  function serializeNodeProperties(node, depth, maxDomDepth, includeShadowTree, contextId, seen, internalIds) {
    const props = {
      nodeType: node.nodeType,
      childNodeCount: node.childNodes ? node.childNodes.length : 0,
    };
    if (node.nodeType === 1) {
      props.localName = node.localName || (node.tagName || '').toLowerCase();
      props.namespaceURI = node.namespaceURI || 'http://www.w3.org/1999/xhtml';
      const attrs = {};
      if (node.attributes) {
        for (let i = 0; i < node.attributes.length; i++) {
          const a = node.attributes[i];
          attrs[a.name] = a.value;
        }
      }
      props.attributes = attrs;
      if (node.shadowRoot && (includeShadowTree === 'all' || (includeShadowTree === 'open' && node.shadowRoot.mode === 'open'))) {
        props.shadowRoot = serializeNode(node.shadowRoot, depth, maxDomDepth, includeShadowTree, contextId, 'none', seen, internalIds);
      } else {
        props.shadowRoot = null;
      }
    } else if (node.nodeType === 2) {
      props.localName = node.localName || node.name || '';
      props.namespaceURI = node.namespaceURI || null;
      props.nodeValue = node.value !== undefined ? String(node.value) : (node.nodeValue || '');
    } else if (node.nodeType === 3 || node.nodeType === 4 || node.nodeType === 7 || node.nodeType === 8) {
      props.nodeValue = node.nodeValue || '';
    } else if (node.nodeType === 10) {
      props.name = node.name || '';
      props.publicId = node.publicId || '';
      props.systemId = node.systemId || '';
    } else if (node.nodeType === 11 && node.host) {
      props.mode = node.mode || 'open';
    }
    if ((maxDomDepth === null || depth < maxDomDepth) && node.childNodes) {
      const children = [];
      for (let i = 0; i < node.childNodes.length; i++) {
        children.push(serializeNode(node.childNodes[i], depth + 1, maxDomDepth, includeShadowTree, contextId, 'none', seen, internalIds));
      }
      props.children = children;
    }
    return props;
  }

  function serializeNode(node, depth, maxDomDepth, includeShadowTree, contextId, ownership, seen, internalIds) {
    const out = {
      type: 'node',
      sharedId: sharedIdFor(node, contextId),
      value: serializeNodeProperties(node, depth, maxDomDepth, includeShadowTree, contextId, seen, internalIds),
    };
    if (ownership === 'root') out.handle = allocHandle(node);
    return out;
  }

  function serialize(value, maxDepth, maxDomDepth, contextId, ownership, seen, internalIds, includeShadowTree) {
    if (includeShadowTree === undefined) includeShadowTree = 'none';
    if (value === undefined) return { type: 'undefined' };
    if (value === null) return { type: 'null' };
    const t = typeof value;
    if (t === 'boolean') return { type: 'boolean', value };
    if (t === 'string') return { type: 'string', value };
    if (t === 'bigint') return { type: 'bigint', value: value.toString() };
    if (t === 'number') {
      if (Number.isNaN(value)) return { type: 'number', value: 'NaN' };
      if (Object.is(value, -0)) return { type: 'number', value: '-0' };
      if (value === Infinity) return { type: 'number', value: 'Infinity' };
      if (value === -Infinity) return { type: 'number', value: '-Infinity' };
      return { type: 'number', value };
    }
    if (t === 'symbol') {
      const out = { type: 'symbol' };
      if (ownership === 'root') out.handle = allocHandle(value);
      return out;
    }
    if (t === 'function') {
      const out = { type: 'function' };
      if (ownership === 'root') out.handle = allocHandle(value);
      return out;
    }
    if (!seen) seen = new Map();
    if (!internalIds) internalIds = { next: 1 };
    const prev = seen.get(value);
    if (prev) {
      if (!prev.internalId) {
        prev.internalId = String(internalIds.next++);
      }
      const dup = { type: prev.type, internalId: prev.internalId };
      if (ownership === 'root' && prev.handle) dup.handle = prev.handle;
      return dup;
    }
    const handle = ownership === 'root' ? allocHandle(value) : undefined;
    if (value === globalThis || (value && value.window === value && typeof value.document === 'object')) {
      const out = { type: 'window', value: { context: contextId } };
      if (handle) out.handle = handle;
      return out;
    }
    if (typeof Node !== 'undefined' && value instanceof Node) {
      return serializeNode(value, 0, maxDomDepth, includeShadowTree, contextId, ownership, seen, internalIds);
    }
    const tag = Object.prototype.toString.call(value);
    if (tag === '[object Generator]' || tag === '[object AsyncGenerator]') {
      const out = { type: 'generator' };
      if (handle) out.handle = handle;
      return out;
    }
    if (value instanceof RegExp) {
      const out = { type: 'regexp', value: { pattern: value.source, flags: value.flags } };
      if (handle) out.handle = handle;
      return out;
    }
    if (value instanceof Date) {
      let iso;
      try { iso = value.toISOString(); } catch (_) { iso = 'Invalid Date'; }
      const out = { type: 'date', value: iso };
      if (handle) out.handle = handle;
      return out;
    }
    if (value instanceof Error) {
      const out = { type: 'error' };
      if (handle) out.handle = handle;
      return out;
    }
    if (value instanceof Promise) {
      const out = { type: 'promise' };
      if (handle) out.handle = handle;
      return out;
    }
    if (value instanceof WeakMap) {
      const out = { type: 'weakmap' };
      if (handle) out.handle = handle;
      return out;
    }
    if (value instanceof WeakSet) {
      const out = { type: 'weakset' };
      if (handle) out.handle = handle;
      return out;
    }
    if (value instanceof ArrayBuffer) {
      const out = { type: 'arraybuffer' };
      if (handle) out.handle = handle;
      return out;
    }
    if (ArrayBuffer.isView(value)) {
      const out = { type: 'typedarray' };
      if (handle) out.handle = handle;
      return out;
    }
    const nextDepth = (maxDepth === null || maxDepth === undefined) ? null : maxDepth - 1;
    const expand = maxDepth === null || maxDepth === undefined || maxDepth > 0;
    if (Array.isArray(value)) {
      const out = { type: 'array' };
      if (handle) out.handle = handle;
      seen.set(value, out);
      if (expand) {
        out.value = value.map(item => serialize(item, nextDepth, maxDomDepth, contextId, 'none', seen, internalIds, includeShadowTree));
      }
      return out;
    }
    if (value instanceof Map) {
      const out = { type: 'map' };
      if (handle) out.handle = handle;
      seen.set(value, out);
      if (expand) {
        const entries = [];
        for (const [k, v] of value.entries()) {
          const sk = typeof k === 'string' ? k : serialize(k, nextDepth, maxDomDepth, contextId, 'none', seen, internalIds, includeShadowTree);
          entries.push([sk, serialize(v, nextDepth, maxDomDepth, contextId, 'none', seen, internalIds, includeShadowTree)]);
        }
        out.value = entries;
      }
      return out;
    }
    if (value instanceof Set) {
      const out = { type: 'set' };
      if (handle) out.handle = handle;
      seen.set(value, out);
      if (expand) {
        const items = [];
        for (const item of value.values()) {
          items.push(serialize(item, nextDepth, maxDomDepth, contextId, 'none', seen, internalIds, includeShadowTree));
        }
        out.value = items;
      }
      return out;
    }
    if (typeof NodeList !== 'undefined' && value instanceof NodeList) {
      const out = { type: 'nodelist' };
      if (handle) out.handle = handle;
      seen.set(value, out);
      if (expand) {
        out.value = Array.from(value, item => serialize(item, nextDepth, maxDomDepth, contextId, 'none', seen, internalIds, includeShadowTree));
      }
      return out;
    }
    if (typeof HTMLCollection !== 'undefined' && value instanceof HTMLCollection) {
      const out = { type: 'htmlcollection' };
      if (handle) out.handle = handle;
      seen.set(value, out);
      if (expand) {
        out.value = Array.from(value, item => serialize(item, nextDepth, maxDomDepth, contextId, 'none', seen, internalIds, includeShadowTree));
      }
      return out;
    }
    const out = { type: 'object' };
    if (handle) out.handle = handle;
    seen.set(value, out);
    if (expand) {
      const entries = [];
      for (const k of Object.keys(value)) {
        entries.push([k, serialize(value[k], nextDepth, maxDomDepth, contextId, 'none', seen, internalIds, includeShadowTree)]);
      }
      out.value = entries;
    }
    return out;
  }

  function deserialize(local, contextId, realmId) {
    if (local === null || typeof local !== 'object') {
      throw new Error('__tb_bidi_invalid:local value must be an object');
    }
    if (typeof local.sharedId === 'string') {
      const node = st.sharedById[local.sharedId];
      if (!node) throw new Error('__tb_bidi_no_such_node:unknown sharedId');
      return node;
    }
    if (typeof local.handle === 'string') {
      if (!(local.handle in st.handles)) throw new Error('__tb_bidi_no_such_handle:unknown handle');
      return st.handles[local.handle];
    }
    switch (local.type) {
      case 'undefined': return undefined;
      case 'null': return null;
      case 'boolean':
        if (typeof local.value !== 'boolean') throw new Error('__tb_bidi_invalid:boolean value');
        return local.value;
      case 'string':
        if (typeof local.value !== 'string') throw new Error('__tb_bidi_invalid:string value');
        return local.value;
      case 'number':
        if (typeof local.value === 'number') return local.value;
        if (local.value === 'NaN') return NaN;
        if (local.value === '-0') return -0;
        if (local.value === 'Infinity') return Infinity;
        if (local.value === '-Infinity') return -Infinity;
        throw new Error('__tb_bidi_invalid:number value');
      case 'bigint':
        if (typeof local.value !== 'string') throw new Error('__tb_bidi_invalid:bigint value');
        return BigInt(local.value);
      case 'date':
        if (typeof local.value !== 'string') throw new Error('__tb_bidi_invalid:date value');
        return new Date(local.value);
      case 'regexp':
        if (!local.value || typeof local.value.pattern !== 'string') throw new Error('__tb_bidi_invalid:regexp');
        return new RegExp(local.value.pattern, local.value.flags || '');
      case 'array':
        if (!Array.isArray(local.value)) throw new Error('__tb_bidi_invalid:array');
        return local.value.map(item => deserialize(item, contextId, realmId));
      case 'set':
        if (!Array.isArray(local.value)) throw new Error('__tb_bidi_invalid:set');
        return new Set(local.value.map(item => deserialize(item, contextId, realmId)));
      case 'map': {
        if (!Array.isArray(local.value)) throw new Error('__tb_bidi_invalid:map');
        const m = new Map();
        for (const pair of local.value) {
          if (!Array.isArray(pair) || pair.length !== 2) throw new Error('__tb_bidi_invalid:map entry');
          const k = typeof pair[0] === 'string' ? pair[0] : deserialize(pair[0], contextId, realmId);
          m.set(k, deserialize(pair[1], contextId, realmId));
        }
        return m;
      }
      case 'object': {
        if (!Array.isArray(local.value)) throw new Error('__tb_bidi_invalid:object');
        const o = {};
        for (const pair of local.value) {
          if (!Array.isArray(pair) || pair.length !== 2) throw new Error('__tb_bidi_invalid:object entry');
          const k = typeof pair[0] === 'string' ? pair[0] : deserialize(pair[0], contextId, realmId);
          o[k] = deserialize(pair[1], contextId, realmId);
        }
        return o;
      }
      case 'channel': {
        const cfg = local.value || {};
        const channelName = cfg.channel;
        if (typeof channelName !== 'string') throw new Error('__tb_bidi_invalid:channel');
        const opts = cfg.serializationOptions || {};
        const maxObjDepth = opts.maxObjectDepth === undefined ? null : opts.maxObjectDepth;
        const maxDomDepth = opts.maxDomDepth === undefined ? 0 : opts.maxDomDepth;
        const own = cfg.ownership || 'none';
        const effectiveRealm = realmId || ('realm-' + contextId);
        return function(data) {
          const payload = JSON.stringify({
            channel: channelName,
            data: serialize(data, maxObjDepth, maxDomDepth, contextId, own),
            realm: effectiveRealm,
            context: contextId,
          });
          if (typeof globalThis.__tbBidiChannelEmit === 'function') {
            globalThis.__tbBidiChannelEmit(payload);
          }
        };
      }
      default:
        throw new Error('__tb_bidi_invalid:unknown local value type ' + String(local.type));
    }
  }

  function exceptionDetails(err, contextId, ownership) {
    const text = err && typeof err === 'object' && err.message !== undefined
      ? (err.name ? `${err.name}: ${err.message}` : String(err.message))
      : String(err);
    return {
      columnNumber: 0,
      exception: serialize(err, 1, 0, contextId, ownership || 'none'),
      lineNumber: 0,
      stackTrace: { callFrames: [] },
      text,
    };
  }

  return { st, serialize, deserialize, exceptionDetails, sharedIdFor };
})();
";
