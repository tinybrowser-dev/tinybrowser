(function() {
  const SLOT = host.slots('tb-security-policy-violation-event');

  function usv(value, fallback) {
    return value === undefined ? fallback : String(value);
  }
  function u16(value, fallback) {
    return value === undefined ? fallback : Number(value) & 0xffff;
  }
  function u32(value, fallback) {
    return value === undefined ? fallback : Number(value) >>> 0;
  }

  // https://w3c.github.io/webappsec-csp/#securitypolicyviolationevent
  function SecurityPolicyViolationEvent(type, init) {
    if (new.target === undefined) {
      throw new TypeError('Class constructor SecurityPolicyViolationEvent cannot be invoked without new');
    }
    if (arguments.length < 1) {
      throw new TypeError('1 argument required, but only 0 present');
    }
    const event = __tbApply(host.__tb_new_security_policy_violation_event, globalThis, arguments);
    // https://webidl.spec.whatwg.org/#interface-object
    if (new.target !== SecurityPolicyViolationEvent) {
      __tbSetPrototypeOf(event, new.target.prototype);
    }
    init = (init === undefined || init === null) ? {} : Object(init);
    let disposition = init.disposition;
    if (disposition === undefined) {
      disposition = 'enforce';
    } else {
      disposition = String(disposition);
      if (disposition !== 'enforce' && disposition !== 'report') {
        throw new TypeError('SecurityPolicyViolationEventInit.disposition: "' + disposition +
          '" is not a valid SecurityPolicyViolationEventDisposition');
      }
    }
    SLOT.set(event, {
      documentURI: usv(init.documentURI, ''),
      referrer: usv(init.referrer, ''),
      blockedURI: usv(init.blockedURI, ''),
      violatedDirective: usv(init.violatedDirective, ''),
      effectiveDirective: usv(init.effectiveDirective, ''),
      originalPolicy: usv(init.originalPolicy, ''),
      sourceFile: usv(init.sourceFile, ''),
      sample: usv(init.sample, ''),
      disposition: disposition,
      statusCode: u16(init.statusCode, 0),
      lineNumber: u32(init.lineNumber, 0),
      columnNumber: u32(init.columnNumber, 0),
    });
    return event;
  }

  const proto = Object.create(globalThis.Event.prototype);
  for (const name of ['documentURI', 'referrer', 'blockedURI', 'violatedDirective',
                      'effectiveDirective', 'originalPolicy', 'sourceFile', 'sample',
                      'disposition', 'statusCode', 'lineNumber', 'columnNumber']) {
    Object.defineProperty(proto, name, {
      get: function() {
        const slot = SLOT.get(this);
        return slot === undefined ? undefined : slot[name];
      },
      enumerable: true,
      configurable: true,
    });
  }
  Object.defineProperty(proto, 'constructor', {
    value: SecurityPolicyViolationEvent,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(SecurityPolicyViolationEvent, 'prototype', { value: proto, writable: false });
  Object.defineProperty(globalThis, 'SecurityPolicyViolationEvent', {
    value: SecurityPolicyViolationEvent,
    writable: true,
    configurable: true,
  });
})();
