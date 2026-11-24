// https://w3c.github.io/hr-time/#the-performance-interface
// https://w3c.github.io/performance-timeline/
// https://w3c.github.io/user-timing/
// https://w3c.github.io/navigation-timing/
// https://w3c.github.io/resource-timing/
{
  const __tbNativePerformance = globalThis.performance;
  const __tbNativeNow = __tbNativePerformance && typeof __tbNativePerformance.now === 'function'
    ? () => __tbNativePerformance.now()
    : () => Date.now();
  // https://w3c.github.io/hr-time/#dfn-current-high-resolution-time
  const __tbResolutionMicros = host.__tbTimeResolutionMicros;
  const __tbClampTime = value => __tbResolutionMicros <= 1
    ? value
    : Math.floor(value * 1000 / __tbResolutionMicros) * __tbResolutionMicros / 1000;
  const __tbNow = () => __tbClampTime(__tbNativeNow());
  // https://w3c.github.io/hr-time/#dom-performance-timeorigin
  const __tbTimeOrigin = Date.now() - __tbNow();

  const __tbEntryData = host.slots('tinybrowser.performanceentry');
  const __tbObserverData = host.slots('tinybrowser.performanceobserver');
  const __tbEntryListData = host.slots('tinybrowser.performanceentrylist');
  const __tbHandlerData = host.slots('tinybrowser.performancehandlers');
  const __tbInternal = __tbObjectCreate(null);

  // https://w3c.github.io/navigation-timing/#sec-PerformanceNavigationTiming
  const __tbIsWindow = typeof globalThis.document === 'object'
    && globalThis.document !== null
    && typeof globalThis.document.createElement === 'function';

  // https://w3c.github.io/performance-timeline/#performance-entry-buffer-map
  const __tbBuffers = __tbObjectCreate(null);
  const __tbRegisterType = (type, maxBufferSize, availableFromTimeline) => {
    __tbBuffers[type] = {
      buffer: [],
      maxBufferSize,
      availableFromTimeline,
      dropped: 0,
      // https://w3c.github.io/resource-timing/#dfn-resource-timing-secondary-buffer
      secondary: [],
      bufferFullPending: false,
    };
  };
  // https://w3c.github.io/timing-entrytypes-registry/#registry
  __tbRegisterType('mark', Infinity, true);
  __tbRegisterType('measure', Infinity, true);
  if (__tbIsWindow) __tbRegisterType('navigation', Infinity, true);
  __tbRegisterType('resource', 250, true);
  const __tbSupportedEntryTypes = __tbFreeze(
    __tbIsWindow
      ? ['mark', 'measure', 'navigation', 'resource']
      : ['mark', 'measure', 'resource'],
  );

  // https://w3c.github.io/user-timing/#dom-performancemark-performancemark
  const __tbTimingNames = __tbFreeze([
    'navigationStart', 'unloadEventStart', 'unloadEventEnd', 'redirectStart',
    'redirectEnd', 'fetchStart', 'domainLookupStart', 'domainLookupEnd',
    'connectStart', 'connectEnd', 'secureConnectionStart', 'requestStart',
    'responseStart', 'responseEnd', 'domLoading', 'domInteractive',
    'domContentLoadedEventStart', 'domContentLoadedEventEnd', 'domComplete',
    'loadEventStart', 'loadEventEnd',
  ]);
  const __tbIsTimingName = name => {
    for (let index = 0; index < __tbTimingNames.length; index++) {
      if (__tbTimingNames[index] === name) return true;
    }
    return false;
  };

  const __tbIsObject = value =>
    (typeof value === 'object' && value !== null) || typeof value === 'function';
  // https://webidl.spec.whatwg.org/#es-dictionary
  const __tbDictionary = (value, what) => {
    if (value === undefined || value === null) return {};
    if (!__tbIsObject(value)) throw new TypeError(what + ' is not a dictionary');
    return value;
  };
  const __tbNumber = (value, what) => {
    const number = Number(value);
    if (!Number.isFinite(number)) throw new TypeError(what + ' is not a finite number');
    return number;
  };
  const __tbNonNegative = (value, what) => {
    const number = __tbNumber(value, what);
    if (number < 0) throw new TypeError(what + ' is negative');
    return number;
  };
  const __tbCloneDetail = value =>
    value === undefined || value === null ? null : structuredClone(value);
  // https://webidl.spec.whatwg.org/#es-sequence
  const __tbIsIterable = value =>
    __tbIsObject(value) && typeof value[__tbIteratorSymbol] === 'function';

  // https://w3c.github.io/performance-timeline/#generate-an-id
  let __tbLastEntryId = 100 + Math.floor(Math.random() * 9900);
  const __tbNextId = () => (__tbLastEntryId += 1);
  // https://w3c.github.io/performance-timeline/#queue-a-performanceentry
  let __tbNavigationId = 0;

  // https://w3c.github.io/performance-timeline/#queue-the-performanceobserver-task
  const __tbScheduleObserver = (observer, data) => {
    if (data.scheduled) return;
    data.scheduled = true;
    Promise.resolve().then(() => {
      data.scheduled = false;
      let droppedEntriesCount = null;
      if (data.requiresDroppedEntries) {
        droppedEntriesCount = 0;
        for (let index = 0; index < data.observed.length; index++) {
          const tuple = __tbBuffers[data.observed[index]];
          if (tuple !== undefined) droppedEntriesCount += tuple.dropped;
        }
        data.requiresDroppedEntries = false;
      }
      const entries = data.records;
      data.records = [];
      // https://w3c.github.io/performance-timeline/#queue-the-performanceobserver-task
      if (entries.length === 0 && (droppedEntriesCount === null || droppedEntriesCount === 0)) {
        return;
      }
      const callbackOptions = {};
      if (droppedEntriesCount !== null) {
        callbackOptions.droppedEntriesCount = droppedEntriesCount;
      }
      const list = new PerformanceObserverEntryList(__tbInternal, entries);
      try {
        data.callback.call(observer, list, observer, callbackOptions);
      } catch (error) {
        // https://w3c.github.io/performance-timeline/#queue-the-performanceobserver-task
        if (typeof globalThis.reportError === 'function') globalThis.reportError(error);
      }
    });
  };
  const __tbObservesType = (data, type) => {
    for (let index = 0; index < data.observed.length; index++) {
      if (data.observed[index] === type) return true;
    }
    return false;
  };
  const __tbNotifyObservers = entry => {
    const entryType = __tbEntryData.get(entry).entryType;
    for (let index = 0; index < __tbObservers.length; index++) {
      const observer = __tbObservers[index];
      const observerData = __tbObserverData.get(observer);
      if (observerData === undefined) continue;
      if (!__tbObservesType(observerData, entryType)) continue;
      __tbArray.push(observerData.records, entry);
      __tbScheduleObserver(observer, observerData);
    }
  };
  // https://w3c.github.io/performance-timeline/#queue-a-performanceentry
  const __tbQueueEntry = (entry, isNavigation) => {
    const data = __tbEntryData.get(entry);
    data.id = __tbNextId();
    if (isNavigation) {
      data.navigationId = data.id;
      if (__tbIsWindow) __tbNavigationId = data.id;
    } else {
      data.navigationId = __tbNavigationId;
    }
    __tbNotifyObservers(entry);
    const tuple = __tbBuffers[data.entryType];
    if (tuple !== undefined) {
      // https://w3c.github.io/performance-timeline/#determine-if-a-performance-entry-buffer-is-full
      if (tuple.buffer.length < tuple.maxBufferSize) {
        __tbArray.push(tuple.buffer, entry);
      } else {
        tuple.dropped += 1;
        if (data.entryType === 'resource') __tbOverflowResource(tuple, entry);
      }
    }
  };

  // https://w3c.github.io/resource-timing/#dfn-add-a-performanceresourcetiming-entry
  const __tbOverflowResource = (tuple, entry) => {
    __tbArray.push(tuple.secondary, entry);
    if (tuple.bufferFullPending) return;
    tuple.bufferFullPending = true;
    setTimeout(() => {
      while (tuple.secondary.length > 0) {
        const before = tuple.secondary.length;
        if (tuple.buffer.length >= tuple.maxBufferSize) {
          const event = new Event('resourcetimingbufferfull');
          __tbPerformanceObject.dispatchEvent(event);
          const handler = __tbHandlerData.get(__tbPerformanceObject);
          if (typeof handler === 'function') handler.call(__tbPerformanceObject, event);
        }
        while (tuple.secondary.length > 0 && tuple.buffer.length < tuple.maxBufferSize) {
          __tbArray.push(tuple.buffer, __tbArray.shift(tuple.secondary));
        }
        if (tuple.secondary.length >= before) {
          tuple.secondary.length = 0;
          break;
        }
      }
      tuple.bufferFullPending = false;
    }, 0);
  };

  // https://webidl.spec.whatwg.org/#default-tojson-operation
  const __tbEntryJSON = (entry, fields) => {
    const data = __tbEntryData.get(entry);
    const json = {
      id: data.id,
      name: data.name,
      entryType: data.entryType,
      startTime: data.startTime,
      duration: data.duration,
      navigationId: data.navigationId,
    };
    for (let index = 0; index < fields.length; index++) {
      json[fields[index]] = data[fields[index]];
    }
    return json;
  };

  // https://w3c.github.io/performance-timeline/#dom-performanceentry
  class PerformanceEntry {
    constructor(token) {
      if (token !== __tbInternal) throw new TypeError('Illegal constructor');
    }
    get id() { return __tbEntryData.get(this).id; }
    get name() { return __tbEntryData.get(this).name; }
    get entryType() { return __tbEntryData.get(this).entryType; }
    get startTime() { return __tbEntryData.get(this).startTime; }
    get duration() { return __tbEntryData.get(this).duration; }
    get navigationId() { return __tbEntryData.get(this).navigationId; }
    toJSON() { return __tbEntryJSON(this, []); }
  }
  __tbDefineProperty(PerformanceEntry.prototype, Symbol.toStringTag, {
    value: 'PerformanceEntry', writable: false, enumerable: false, configurable: true,
  });

  // https://w3c.github.io/user-timing/#the-performancemark-interface
  class PerformanceMark extends PerformanceEntry {
    constructor(markName, markOptions) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to construct 'PerformanceMark': 1 argument required, but only 0 present.");
      }
      const name = String(markName);
      // https://w3c.github.io/user-timing/#dom-performancemark-performancemark
      if (__tbIsWindow && __tbIsTimingName(name)) {
        throw new DOMException("'" + name + "' is a PerformanceTiming attribute", 'SyntaxError');
      }
      super(__tbInternal);
      const options = __tbDictionary(markOptions, 'PerformanceMarkOptions');
      const startTime = options.startTime === undefined
        ? __tbNow()
        : __tbNonNegative(options.startTime, 'startTime');
      __tbEntryData.set(this, {
        id: 0,
        name,
        entryType: 'mark',
        startTime,
        duration: 0,
        navigationId: 0,
        detail: __tbCloneDetail(options.detail),
      });
    }
    get detail() { return __tbEntryData.get(this).detail; }
    toJSON() { return __tbEntryJSON(this, ['detail']); }
  }
  __tbDefineProperty(PerformanceMark.prototype, Symbol.toStringTag, {
    value: 'PerformanceMark', writable: false, enumerable: false, configurable: true,
  });

  // https://w3c.github.io/user-timing/#the-performancemeasure-interface
  class PerformanceMeasure extends PerformanceEntry {
    constructor(token, name, startTime, duration, detail) {
      if (token !== __tbInternal) throw new TypeError('Illegal constructor');
      super(token);
      __tbEntryData.set(this, {
        id: 0, name, entryType: 'measure', startTime, duration, navigationId: 0, detail,
      });
    }
    get detail() { return __tbEntryData.get(this).detail; }
    toJSON() { return __tbEntryJSON(this, ['detail']); }
  }
  __tbDefineProperty(PerformanceMeasure.prototype, Symbol.toStringTag, {
    value: 'PerformanceMeasure', writable: false, enumerable: false, configurable: true,
  });

  // https://w3c.github.io/resource-timing/#sec-performanceresourcetiming
  const __tbResourceFields = __tbFreeze([
    'initiatorType', 'deliveryType', 'nextHopProtocol', 'workerStart',
    'redirectStart', 'redirectEnd', 'fetchStart', 'domainLookupStart',
    'domainLookupEnd', 'connectStart', 'connectEnd', 'secureConnectionStart',
    'requestStart', 'finalResponseHeadersStart', 'firstInterimResponseStart',
    'responseStart', 'responseEnd', 'workerRouterEvaluationStart',
    'workerCacheLookupStart', 'workerMatchedRouterSource',
    'workerFinalRouterSource', 'transferSize', 'encodedBodySize',
    'decodedBodySize', 'responseStatus', 'renderBlockingStatus',
    'contentType', 'contentEncoding',
  ]);
  class PerformanceResourceTiming extends PerformanceEntry {
    constructor(token, data) {
      if (token !== __tbInternal) throw new TypeError('Illegal constructor');
      super(token);
      __tbEntryData.set(this, data);
    }
    toJSON() { return __tbEntryJSON(this, __tbResourceFields); }
  }
  for (let index = 0; index < __tbResourceFields.length; index++) {
    const field = __tbResourceFields[index];
    __tbDefineProperty(PerformanceResourceTiming.prototype, field, {
      get() { return __tbEntryData.get(this)[field]; },
      enumerable: true,
      configurable: true,
    });
  }
  __tbDefineProperty(PerformanceResourceTiming.prototype, Symbol.toStringTag, {
    value: 'PerformanceResourceTiming', writable: false, enumerable: false, configurable: true,
  });

  // https://w3c.github.io/resource-timing/#attribute-descriptions
  // https://fetch.spec.whatwg.org/#concept-tao-check
  const __tbResourceEntry = (name, initiatorType, startTime, endTime, responseStatus, encodedBodySize, decodedBodySize, taoPassed) => {
    const allowed = taoPassed !== false;
    return new PerformanceResourceTiming(__tbInternal, {
      id: 0,
      name,
      entryType: 'resource',
      startTime,
      duration: endTime - startTime,
      navigationId: 0,
      initiatorType,
      deliveryType: '',
      nextHopProtocol: '',
      workerStart: 0,
      redirectStart: 0,
      redirectEnd: 0,
      fetchStart: startTime,
      domainLookupStart: 0,
      domainLookupEnd: 0,
      connectStart: 0,
      connectEnd: 0,
      secureConnectionStart: 0,
      requestStart: allowed ? startTime : 0,
      finalResponseHeadersStart: allowed ? endTime : 0,
      firstInterimResponseStart: 0,
      // https://w3c.github.io/resource-timing/#dom-performanceresourcetiming-responsestart
      responseStart: allowed ? endTime : 0,
      responseEnd: endTime,
      workerRouterEvaluationStart: 0,
      workerCacheLookupStart: 0,
      workerMatchedRouterSource: '',
      workerFinalRouterSource: '',
      transferSize: !allowed || encodedBodySize === 0 ? 0 : encodedBodySize + 300,
      encodedBodySize: allowed ? encodedBodySize : 0,
      decodedBodySize: allowed ? decodedBodySize : 0,
      responseStatus: allowed ? responseStatus : 0,
      renderBlockingStatus: 'non-blocking',
      contentType: '',
      contentEncoding: '',
    });
  };

  // https://w3c.github.io/user-timing/#dom-performance-measure
  const __tbConvertMark = mark => {
    if (typeof mark === 'string') {
      if (__tbIsWindow && __tbIsTimingName(mark)) {
        // https://w3c.github.io/user-timing/#convert-a-name-to-a-timestamp
        if (mark === 'navigationStart') return 0;
        return __tbTimingValue(mark);
      }
      let found;
      for (let index = 0; index < __tbBuffers.mark.buffer.length; index++) {
        const entry = __tbBuffers.mark.buffer[index];
        if (entry.name === mark) found = entry;
      }
      if (found === undefined) {
        throw new DOMException("'" + mark + "' is not a mark", 'SyntaxError');
      }
      return found.startTime;
    }
    const time = __tbNumber(mark, 'mark');
    if (time < 0) throw new TypeError('mark is negative');
    return time;
  };
  const __tbRemoveEntries = (entryType, name) => {
    const tuple = __tbBuffers[entryType];
    if (tuple === undefined) return;
    for (let index = tuple.buffer.length - 1; index >= 0; index--) {
      const entry = tuple.buffer[index];
      if (name !== undefined && entry.name !== name) continue;
      __tbArray.remove(tuple.buffer, index);
    }
  };

  // https://w3c.github.io/navigation-timing/#sec-PerformanceTiming
  const __tbNavigationStart = Math.floor(__tbTimeOrigin);
  const __tbTimingValue = name => {
    void name;
    return 0;
  };
  class PerformanceTiming {
    constructor(token) {
      if (token !== __tbInternal) throw new TypeError('Illegal constructor');
    }
    toJSON() {
      const json = {};
      for (let index = 0; index < __tbTimingNames.length; index++) {
        json[__tbTimingNames[index]] = __tbNavigationStart;
      }
      return json;
    }
  }
  for (let index = 0; index < __tbTimingNames.length; index++) {
    __tbDefineProperty(PerformanceTiming.prototype, __tbTimingNames[index], {
      get() { return __tbNavigationStart; },
      enumerable: true,
      configurable: true,
    });
  }
  __tbDefineProperty(PerformanceTiming.prototype, Symbol.toStringTag, {
    value: 'PerformanceTiming', writable: false, enumerable: false, configurable: true,
  });
  // https://w3c.github.io/navigation-timing/#sec-PerformanceNavigation
  class PerformanceNavigation {
    constructor(token) {
      if (token !== __tbInternal) throw new TypeError('Illegal constructor');
    }
    get type() { return 0; }
    get redirectCount() { return 0; }
    toJSON() { return { type: 0, redirectCount: 0 }; }
  }
  for (const [name, value] of [
    ['TYPE_NAVIGATE', 0],
    ['TYPE_RELOAD', 1],
    ['TYPE_BACK_FORWARD', 2],
    ['TYPE_RESERVED', 255],
  ]) {
    __tbDefineProperty(PerformanceNavigation, name, { value, enumerable: true });
    __tbDefineProperty(PerformanceNavigation.prototype, name, { value, enumerable: true });
  }
  __tbDefineProperty(PerformanceNavigation.prototype, Symbol.toStringTag, {
    value: 'PerformanceNavigation', writable: false, enumerable: false, configurable: true,
  });

  // https://w3c.github.io/navigation-timing/#performancenavigationtiming
  class PerformanceNavigationTiming extends PerformanceResourceTiming {
    constructor(token, data) {
      if (token !== __tbInternal) throw new TypeError('Illegal constructor');
      super(token, data);
    }
    toJSON() {
      return __tbEntryJSON(this, __tbNavigationTimingFields);
    }
  }
  const __tbNavigationTimingFields = __tbFreeze([
    ...__tbResourceFields,
    'unloadEventStart', 'unloadEventEnd', 'domInteractive',
    'domContentLoadedEventStart', 'domContentLoadedEventEnd', 'domComplete',
    'loadEventStart', 'loadEventEnd', 'type', 'redirectCount', 'criticalCHRestart',
  ]);
  for (let index = 0; index < __tbNavigationTimingFields.length; index++) {
    const field = __tbNavigationTimingFields[index];
    if (__tbResourceFields.indexOf(field) !== -1) continue;
    __tbDefineProperty(PerformanceNavigationTiming.prototype, field, {
      get() { return __tbEntryData.get(this)[field]; },
      enumerable: true,
      configurable: true,
    });
  }
  __tbDefineProperty(PerformanceNavigationTiming.prototype, 'notRestoredReasons', {
    get() { return null; }, enumerable: true, configurable: true,
  });
  __tbDefineProperty(PerformanceNavigationTiming.prototype, 'confidence', {
    get() { return null; }, enumerable: true, configurable: true,
  });
  __tbDefineProperty(PerformanceNavigationTiming.prototype, Symbol.toStringTag, {
    value: 'PerformanceNavigationTiming', writable: false, enumerable: false, configurable: true,
  });

  // https://w3c.github.io/hr-time/#the-performance-interface
  const __tbTiming = __tbIsWindow ? new PerformanceTiming(__tbInternal) : null;
  const __tbNavigation = __tbIsWindow ? new PerformanceNavigation(__tbInternal) : null;
  const __tbTimeline = () => __tbFilter(null, null);
  const __tbFilter = (name, type) => {
    const entries = [];
    for (const entryType in __tbBuffers) {
      const tuple = __tbBuffers[entryType];
      if (!tuple.availableFromTimeline) continue;
      if (type !== null && entryType !== type) continue;
      for (let index = 0; index < tuple.buffer.length; index++) {
        const entry = tuple.buffer[index];
        if (name !== null && entry.name !== name) continue;
        __tbArray.push(entries, entry);
      }
    }
    // https://w3c.github.io/performance-timeline/#dfn-filter-buffer-map-by-name-and-type
    __tbArray.sort(entries, (left, right) => left.startTime - right.startTime);
    return entries;
  };

  class Performance extends EventTarget {
    constructor(token) {
      if (token !== __tbInternal) throw new TypeError('Illegal constructor');
      super();
    }
    now() { return __tbNow(); }
    get timeOrigin() { return __tbTimeOrigin; }
    getEntries() { return __tbTimeline(); }
    getEntriesByType(type) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'getEntriesByType' on 'Performance': 1 argument required, but only 0 present.");
      }
      return __tbFilter(null, String(type));
    }
    getEntriesByName(name, type) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'getEntriesByName' on 'Performance': 1 argument required, but only 0 present.");
      }
      return __tbFilter(String(name), type === undefined ? null : String(type));
    }
    // https://w3c.github.io/user-timing/#dom-performance-mark
    mark(markName, markOptions) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'mark' on 'Performance': 1 argument required, but only 0 present.");
      }
      const entry = new PerformanceMark(markName, markOptions);
      __tbQueueEntry(entry, false);
      return entry;
    }
    clearMarks(markName) {
      __tbRemoveEntries('mark', markName === undefined ? undefined : String(markName));
    }
    // https://w3c.github.io/user-timing/#dom-performance-measure
    measure(measureName, startOrMeasureOptions, endMark) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'measure' on 'Performance': 1 argument required, but only 0 present.");
      }
      const name = String(measureName);
      const endMarkGiven = arguments.length > 2 && endMark !== undefined;
      let options = null;
      let startOption = false;
      let endOption = false;
      let durationOption = false;
      let detailOption = false;
      let startIsString = false;
      if (typeof startOrMeasureOptions === 'string') {
        startIsString = true;
      } else if (__tbIsObject(startOrMeasureOptions)) {
        options = startOrMeasureOptions;
        startOption = options.start !== undefined;
        endOption = options.end !== undefined;
        durationOption = options.duration !== undefined;
        detailOption = options.detail !== undefined;
      } else if (startOrMeasureOptions !== undefined && startOrMeasureOptions !== null) {
        startIsString = true;
      }
      if (options !== null && (startOption || endOption || durationOption || detailOption)) {
        if (endMarkGiven) throw new TypeError('endMark is not allowed with a measure options object');
        if (!startOption && !endOption) throw new TypeError('start or end is required');
        if (startOption && durationOption && endOption) {
          throw new TypeError('start, duration, and end cannot all be present');
        }
      }
      let endTime;
      if (endMarkGiven) {
        endTime = __tbConvertMark(endMark);
      } else if (options !== null && endOption) {
        endTime = __tbConvertMark(options.end);
      } else if (options !== null && startOption && durationOption) {
        endTime = __tbConvertMark(options.start) + __tbConvertMark(options.duration);
      } else {
        endTime = __tbNow();
      }
      let startTime;
      if (options !== null && startOption) {
        startTime = __tbConvertMark(options.start);
      } else if (options !== null && durationOption && endOption) {
        startTime = __tbConvertMark(options.end) - __tbConvertMark(options.duration);
      } else if (startIsString) {
        startTime = __tbConvertMark(String(startOrMeasureOptions));
      } else {
        startTime = 0;
      }
      const detail = options !== null && detailOption ? __tbCloneDetail(options.detail) : null;
      const entry = new PerformanceMeasure(__tbInternal, name, startTime, endTime - startTime, detail);
      __tbQueueEntry(entry, false);
      return entry;
    }
    clearMeasures(measureName) {
      __tbRemoveEntries('measure', measureName === undefined ? undefined : String(measureName));
    }
    // https://w3c.github.io/resource-timing/#dom-performance-clearresourcetimings
    clearResourceTimings() {
      __tbRemoveEntries('resource');
    }
    // https://w3c.github.io/resource-timing/#dom-performance-setresourcetimingbuffersize
    setResourceTimingBufferSize(maxSize) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'setResourceTimingBufferSize' on 'Performance': 1 argument required, but only 0 present.");
      }
      __tbBuffers.resource.maxBufferSize = Number(maxSize) >>> 0;
    }
    get onresourcetimingbufferfull() {
      const handler = __tbHandlerData.get(this);
      return handler === undefined ? null : handler;
    }
    set onresourcetimingbufferfull(value) {
      __tbHandlerData.set(this, typeof value === 'function' ? value : null);
    }
    // https://www.w3.org/TR/navigation-timing/#sec-navigation-timing-interface
    get timing() {
      const t = Math.floor(__tbTimeOrigin);
      return {
        navigationStart: t,
        unloadEventStart: 0,
        unloadEventEnd: 0,
        redirectStart: 0,
        redirectEnd: 0,
        fetchStart: t,
        domainLookupStart: t,
        domainLookupEnd: t,
        connectStart: t,
        connectEnd: t,
        secureConnectionStart: 0,
        requestStart: t,
        responseStart: t,
        responseEnd: t,
        domLoading: t,
        domInteractive: t,
        domContentLoadedEventStart: t,
        domContentLoadedEventEnd: t,
        domComplete: t,
        loadEventStart: t,
        loadEventEnd: t,
      };
    }
    toJSON() { return { timeOrigin: __tbTimeOrigin, timing: this.timing }; }
  }
  __tbDefineProperty(Performance.prototype, Symbol.toStringTag, {
    value: 'Performance', writable: false, enumerable: false, configurable: true,
  });

  // https://w3c.github.io/performance-timeline/#performanceobserverentrylist
  class PerformanceObserverEntryList {
    constructor(token, entries) {
      if (token !== __tbInternal) throw new TypeError('Illegal constructor');
      __tbEntryListData.set(this, entries);
    }
    getEntries() { return __tbEntryListData.get(this).slice(); }
    getEntriesByType(type) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'getEntriesByType' on 'PerformanceObserverEntryList': 1 argument required, but only 0 present.");
      }
      const wanted = String(type);
      const entries = [];
      const all = __tbEntryListData.get(this);
      for (let index = 0; index < all.length; index++) {
        if (all[index].entryType === wanted) __tbArray.push(entries, all[index]);
      }
      return entries;
    }
    getEntriesByName(name, type) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'getEntriesByName' on 'PerformanceObserverEntryList': 1 argument required, but only 0 present.");
      }
      const wantedName = String(name);
      const wantedType = type === undefined ? undefined : String(type);
      const entries = [];
      const all = __tbEntryListData.get(this);
      for (let index = 0; index < all.length; index++) {
        const entry = all[index];
        if (entry.name !== wantedName) continue;
        if (wantedType !== undefined && entry.entryType !== wantedType) continue;
        __tbArray.push(entries, entry);
      }
      return entries;
    }
  }
  __tbDefineProperty(PerformanceObserverEntryList.prototype, Symbol.toStringTag, {
    value: 'PerformanceObserverEntryList', writable: false, enumerable: false, configurable: true,
  });

  // https://w3c.github.io/performance-timeline/#the-performanceobserver-interface
  const __tbObservers = [];
  class PerformanceObserver {
    constructor(callback) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to construct 'PerformanceObserver': 1 argument required, but only 0 present.");
      }
      if (typeof callback !== 'function') {
        throw new TypeError("Failed to construct 'PerformanceObserver': callback is not a function");
      }
      __tbObserverData.set(this, {
        observer: this,
        callback,
        observed: [],
        records: [],
        scheduled: false,
        requiresDroppedEntries: false,
      });
      __tbArray.push(__tbObservers, this);
    }
    observe(options) {
      const data = __tbObserverData.get(this);
      if (data === undefined) throw new TypeError('Illegal invocation');
      if (!__tbIsObject(options)) {
        throw new TypeError("Failed to execute 'observe' on 'PerformanceObserver': options is not a dictionary");
      }
      const hasEntryTypes = options.entryTypes !== undefined;
      const hasType = options.type !== undefined;
      if (hasEntryTypes && hasType) {
        throw new TypeError('type and entryTypes cannot both be present');
      }
      if (!hasEntryTypes && !hasType) {
        throw new TypeError('type or entryTypes is required');
      }
      const buffered = options.buffered === true;
      // https://w3c.github.io/performance-timeline/#dom-performanceobserver-observe
      if (data.multiple === undefined) data.multiple = hasEntryTypes;
      if (data.multiple !== hasEntryTypes) {
        throw new DOMException(
          hasEntryTypes ? 'observer is already observing a type' : 'observer is already observing entryTypes',
          'InvalidModificationError',
        );
      }
      data.requiresDroppedEntries = true;
      const types = [];
      if (hasEntryTypes) {
        if (!__tbIsIterable(options.entryTypes)) {
          throw new TypeError('entryTypes is not a sequence');
        }
        for (const value of options.entryTypes) {
          const type = String(value);
          if (__tbSupportedEntryTypes.indexOf(type) !== -1 && types.indexOf(type) === -1) {
            __tbArray.push(types, type);
          }
        }
        // https://w3c.github.io/performance-timeline/#dom-performanceobserver-observe
        data.observed = types;
      } else {
        const type = String(options.type);
        if (__tbSupportedEntryTypes.indexOf(type) !== -1 && !__tbObservesType(data, type)) {
          __tbArray.push(data.observed, type);
        }
        if (buffered) {
          const tuple = __tbBuffers[type];
          if (tuple !== undefined) {
            for (let index = 0; index < tuple.buffer.length; index++) {
              __tbArray.push(data.records, tuple.buffer[index]);
            }
          }
        }
      }
      if (buffered && hasEntryTypes) {
        for (let index = 0; index < types.length; index++) {
          const tuple = __tbBuffers[types[index]];
          if (tuple === undefined) continue;
          for (let entryIndex = 0; entryIndex < tuple.buffer.length; entryIndex++) {
            __tbArray.push(data.records, tuple.buffer[entryIndex]);
          }
        }
      }
      // https://w3c.github.io/performance-timeline/#dom-performanceobserver-observe
      if (buffered) __tbScheduleObserver(this, data);
    }
    disconnect() {
      const data = __tbObserverData.get(this);
      if (data === undefined) throw new TypeError('Illegal invocation');
      data.observed = [];
      data.records = [];
      data.scheduled = false;
    }
    takeRecords() {
      const data = __tbObserverData.get(this);
      if (data === undefined) throw new TypeError('Illegal invocation');
      const records = data.records;
      data.records = [];
      return records;
    }
    static get supportedEntryTypes() { return __tbSupportedEntryTypes; }
  }
  __tbDefineProperty(PerformanceObserver, 'length', { value: 1, configurable: true });
  __tbDefineProperty(PerformanceObserver.prototype, Symbol.toStringTag, {
    value: 'PerformanceObserver', writable: false, enumerable: false, configurable: true,
  });

  const __tbPerformanceObject = new Performance(__tbInternal);

  // https://w3c.github.io/navigation-timing/#marking-navigation-timing
  let __tbNavigationEntry = null;
  if (__tbIsWindow) {
    __tbNavigationEntry = new PerformanceNavigationTiming(__tbInternal, {
      id: 0,
      name: String(globalThis.location.href),
      entryType: 'navigation',
      startTime: 0,
      duration: 0,
      navigationId: 0,
      initiatorType: 'navigation',
      deliveryType: '',
      nextHopProtocol: '',
      workerStart: 0,
      redirectStart: 0,
      redirectEnd: 0,
      fetchStart: 0,
      domainLookupStart: 0,
      domainLookupEnd: 0,
      connectStart: 0,
      connectEnd: 0,
      secureConnectionStart: 0,
      requestStart: 0,
      finalResponseHeadersStart: 0,
      firstInterimResponseStart: 0,
      responseStart: 0,
      responseEnd: 0,
      workerRouterEvaluationStart: 0,
      workerCacheLookupStart: 0,
      workerMatchedRouterSource: '',
      workerFinalRouterSource: '',
      transferSize: 0,
      encodedBodySize: 0,
      decodedBodySize: 0,
      responseStatus: 200,
      renderBlockingStatus: 'non-blocking',
      contentType: '',
      contentEncoding: '',
      unloadEventStart: 0,
      unloadEventEnd: 0,
      domInteractive: 0,
      domContentLoadedEventStart: 0,
      domContentLoadedEventEnd: 0,
      domComplete: 0,
      loadEventStart: 0,
      loadEventEnd: 0,
      type: 'navigate',
      redirectCount: 0,
      criticalCHRestart: 0,
    });
    const navigationData = __tbEntryData.get(__tbNavigationEntry);
    navigationData.id = __tbNextId();
    navigationData.navigationId = navigationData.id;
    __tbNavigationId = navigationData.id;
    __tbArray.push(__tbBuffers.navigation.buffer, __tbNavigationEntry);
  }
  host.__tbQueueNavigationTiming = () => {
    if (__tbNavigationEntry === null) return;
    const navigationData = __tbEntryData.get(__tbNavigationEntry);
    // https://w3c.github.io/navigation-timing/#performancenavigationtiming
    navigationData.loadEventEnd = __tbNow();
    navigationData.loadEventStart = navigationData.loadEventEnd;
    navigationData.domComplete = navigationData.loadEventEnd;
    navigationData.duration = navigationData.loadEventEnd;
    __tbNotifyObservers(__tbNavigationEntry);
  };

  // https://w3c.github.io/resource-timing/#dfn-add-a-performanceresourcetiming-entry
  host.__tbQueueResourceTiming = (
    name, initiatorType, startTime, endTime, responseStatus, encodedBodySize, decodedBodySize, taoPassed,
  ) => {
    const entry = __tbResourceEntry(
      name, initiatorType, startTime, endTime, responseStatus,
      encodedBodySize, decodedBodySize, taoPassed,
    );
    __tbQueueEntry(entry, false);
  };

  for (const [name, value] of [
    ['PerformanceEntry', PerformanceEntry],
    ['PerformanceMark', PerformanceMark],
    ['PerformanceMeasure', PerformanceMeasure],
    ['PerformanceResourceTiming', PerformanceResourceTiming],
    ['PerformanceObserverEntryList', PerformanceObserverEntryList],
    ['PerformanceObserver', PerformanceObserver],
    ['Performance', Performance],
  ]) {
    __tbDefineProperty(globalThis, name, {
      value, writable: true, enumerable: false, configurable: true,
    });
  }
  if (__tbIsWindow) {
    // https://w3c.github.io/navigation-timing/#sec-PerformanceNavigationTiming
    __tbDefineProperty(Performance.prototype, 'timing', {
      get() { return __tbTiming; }, enumerable: true, configurable: true,
    });
    __tbDefineProperty(Performance.prototype, 'navigation', {
      get() { return __tbNavigation; }, enumerable: true, configurable: true,
    });
    __tbDefineProperty(globalThis, 'PerformanceTiming', {
      value: PerformanceTiming, writable: true, enumerable: false, configurable: true,
    });
    __tbDefineProperty(globalThis, 'PerformanceNavigation', {
      value: PerformanceNavigation, writable: true, enumerable: false, configurable: true,
    });
    __tbDefineProperty(globalThis, 'PerformanceNavigationTiming', {
      value: PerformanceNavigationTiming, writable: true, enumerable: false, configurable: true,
    });
  }
  __tbDefineProperty(globalThis, 'performance', {
    value: __tbPerformanceObject, writable: true, enumerable: true, configurable: true,
  });
}
