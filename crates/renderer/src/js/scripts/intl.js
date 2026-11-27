(function() {
  const nativeCanonicalLocale = host.__tbIntlCanonicalLocale;
  const nativeResolveLocale = host.__tbIntlResolveLocale;
  const nativeCurrencyDigits = host.__tbIntlCurrencyDigits;
  const nativeFormatNumber = host.__tbIntlFormatNumber;
  const nativeFormatDateTime = host.__tbIntlFormatDateTime;
  delete host.__tbIntlCanonicalLocale;
  delete host.__tbIntlResolveLocale;
  delete host.__tbIntlCurrencyDigits;
  delete host.__tbIntlFormatNumber;
  delete host.__tbIntlFormatDateTime;

  const defaultLocale = host.__tbIdentity().languages[0];

  const call = Function.prototype.call.bind(Function.prototype.call);
  const objectConstructor = Object;
  const objectCreate = Object.create;
  const stringConstructor = String;
  const stringToUpperCase = Function.prototype.call.bind(String.prototype.toUpperCase);
  const stringToLowerCase = Function.prototype.call.bind(String.prototype.toLowerCase);
  const stringTrim = Function.prototype.call.bind(String.prototype.trim);
  const numberIsFinite = Number.isFinite;
  const numberMaxSafeInteger = Number.MAX_SAFE_INTEGER;
  const booleanConstructor = Boolean;
  const numberValueOf = Number.prototype.valueOf;
  const bigintValueOf = BigInt.prototype.valueOf;
  const symbolToPrimitive = Symbol.toPrimitive;
  const arrayIndexOf = Function.prototype.call.bind(Array.prototype.indexOf);
  const arrayPush = Function.prototype.call.bind(Array.prototype.push);
  const arrayConcat = Function.prototype.call.bind(Array.prototype.concat);
  const regexpTest = Function.prototype.call.bind(RegExp.prototype.test);
  const stringSlice = Function.prototype.call.bind(String.prototype.slice);
  const stringIndexOf = Function.prototype.call.bind(String.prototype.indexOf);
  const stringCharCodeAt = Function.prototype.call.bind(String.prototype.charCodeAt);
  const stringRepeat = Function.prototype.call.bind(String.prototype.repeat);
  const bigintConstructor = BigInt;
  const mathFloor = Math.floor;
  const mathMax = Math.max;
  const mathMin = Math.min;
  const dateConstructor = Date;
  const dateNow = Date.now;
  const dateValueOf = Date.prototype.valueOf;
  const dateGetUTCFullYear = Date.prototype.getUTCFullYear;
  const dateGetUTCMonth = Date.prototype.getUTCMonth;
  const dateGetUTCDate = Date.prototype.getUTCDate;
  const dateGetUTCHours = Date.prototype.getUTCHours;
  const dateGetUTCMinutes = Date.prototype.getUTCMinutes;
  const dateGetUTCSeconds = Date.prototype.getUTCSeconds;

  const numberSlots = host.slots();
  const dateTimeSlots = host.slots();
  const segmenterSlots = host.slots();
  const segmentsSlots = host.slots();
  const segmentIterSlots = host.slots();
  const numberSlotsGet = numberSlots.get.bind(numberSlots);
  const numberSlotsSet = numberSlots.set.bind(numberSlots);
  const dateTimeSlotsGet = dateTimeSlots.get.bind(dateTimeSlots);
  const dateTimeSlotsSet = dateTimeSlots.set.bind(dateTimeSlots);
  const segmenterSlotsGet = segmenterSlots.get.bind(segmenterSlots);
  const segmenterSlotsSet = segmenterSlots.set.bind(segmenterSlots);
  const segmentsSlotsGet = segmentsSlots.get.bind(segmentsSlots);
  const segmentsSlotsSet = segmentsSlots.set.bind(segmentsSlots);
  const segmentIterSlotsGet = segmentIterSlots.get.bind(segmentIterSlots);
  const segmentIterSlotsSet = segmentIterSlots.set.bind(segmentIterSlots);

  // https://402.ecma-international.org/#sec-canonicalizelocalelist
  function localeList(locales) {
    if (locales === undefined) return [];
    if (typeof locales === 'string') return [locales];
    if (locales === null) throw new TypeError('locales must not be null');
    const object = objectConstructor(locales);
    const numericLength = +object.length;
    const length = mathMin(
      mathMax(numberIsFinite(numericLength) ? mathFloor(numericLength) : 0, 0),
      numberMaxSafeInteger
    );
    const result = [];
    for (let index = 0; index < length; index += 1) {
      if (!(index in object)) continue;
      const value = object[index];
      if (value === null || (typeof value !== 'string' && typeof value !== 'object' &&
          typeof value !== 'function')) {
        throw new TypeError('locale list elements must be strings or objects');
      }
      arrayPush(result, stringConstructor(value));
    }
    return result;
  }

  function canonicalLocaleList(locales) {
    const requested = localeList(locales);
    const result = [];
    for (let index = 0; index < requested.length; index += 1) {
      const tag = requested[index];
      const canonical = nativeCanonicalLocale(stringConstructor(tag));
      if (canonical === '!') throw new RangeError('invalid language tag: ' + tag);
      if (arrayIndexOf(result, canonical) < 0) arrayPush(result, canonical);
    }
    return result;
  }

  function resolveLocale(locales) {
    const requested = canonicalLocaleList(locales);
    for (let index = 0; index < requested.length; index += 1) {
      const resolved = nativeResolveLocale(requested[index]);
      if (resolved) return resolved;
    }
    return defaultLocale;
  }

  // https://402.ecma-international.org/#sec-supportedlocales
  function supportedLocales(locales, options) {
    options = optionsObject(options);
    stringOption(options, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
    const result = [];
    const requested = canonicalLocaleList(locales);
    for (let index = 0; index < requested.length; index += 1) {
      const canonical = requested[index];
      if (nativeResolveLocale(canonical) && arrayIndexOf(result, canonical) < 0) {
        arrayPush(result, canonical);
      }
    }
    return result;
  }

  function optionsObject(options) {
    if (options === undefined) return objectCreate(null);
    if (options === null) throw new TypeError('options must be an object');
    return objectConstructor(options);
  }

  function stringOption(options, name, values, fallback) {
    const value = options[name];
    if (value === undefined) return fallback;
    const text = stringConstructor(value);
    if (arrayIndexOf(values, text) < 0) throw new RangeError('invalid ' + name);
    return text;
  }

  function digitOption(options, name, fallback, minimum, maximum) {
    const value = options[name];
    if (value === undefined) return fallback;
    const integer = mathFloor(+value);
    if (!numberIsFinite(integer) || integer < minimum || integer > maximum) {
      throw new RangeError('invalid ' + name);
    }
    return integer;
  }

  function primitiveNumberHint(value) {
    if (value === null || (typeof value !== 'object' && typeof value !== 'function')) {
      return value;
    }
    const exotic = value[symbolToPrimitive];
    if (exotic !== undefined) {
      if (typeof exotic !== 'function') throw new TypeError('@@toPrimitive must be callable');
      const primitive = call(exotic, value, 'number');
      if (primitive === null || (typeof primitive !== 'object' &&
          typeof primitive !== 'function')) return primitive;
      throw new TypeError('@@toPrimitive must return a primitive');
    }
    const methods = ['valueOf', 'toString'];
    for (let index = 0; index < methods.length; index += 1) {
      const method = value[methods[index]];
      if (typeof method !== 'function') continue;
      const primitive = call(method, value);
      if (primitive === null || (typeof primitive !== 'object' &&
          typeof primitive !== 'function')) return primitive;
    }
    throw new TypeError('cannot convert object to a primitive value');
  }

  // https://402.ecma-international.org/#sec-tointlmathematicalvalue
  // https://262.ecma-international.org/#sec-stringnumericliteral
  function expandDecimalLiteral(text) {
    if (text.length > 2 && text[0] === '0') {
      const prefix = text[1] === 'x' || text[1] === 'X' ? [16, /^[0-9a-fA-F]+$/]
        : text[1] === 'b' || text[1] === 'B' ? [2, /^[01]+$/]
        : text[1] === 'o' || text[1] === 'O' ? [8, /^[0-7]+$/] : null;
      if (prefix) {
        if (!regexpTest(prefix[1], stringSlice(text, 2))) return null;
        return stringConstructor(bigintConstructor('0' + stringToLowerCase(text[1]) + stringSlice(text, 2)));
      }
    }
    const lower = stringIndexOf(text, 'e');
    const upper = stringIndexOf(text, 'E');
    const marker = lower < 0 ? upper : upper < 0 ? lower : mathMin(lower, upper);
    if (marker < 0) return null;
    const mantissa = stringSlice(text, 0, marker);
    if (!regexpTest(/^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)$/, mantissa)) return null;
    const exponent = parseDecimalExponent(stringSlice(text, marker + 1));
    if (exponent === null || exponent > 9999 || exponent < -9999) return null;
    return expandDecimalExponent(mantissa, exponent);
  }

  function parseDecimalExponent(text) {
    let sign = 1;
    let rest = text;
    if (rest[0] === '+') rest = stringSlice(rest, 1);
    else if (rest[0] === '-') { sign = -1; rest = stringSlice(rest, 1); }
    if (!regexpTest(/^[0-9]+$/, rest)) return null;
    let value = 0;
    for (let index = 0; index < rest.length; index += 1) {
      value = value * 10 + (stringCharCodeAt(rest, index) - 48);
    }
    return sign * value;
  }

  function expandDecimalExponent(mantissa, exponent) {
    let sign = '';
    if (mantissa[0] === '+' || mantissa[0] === '-') {
      if (mantissa[0] === '-') sign = '-';
      mantissa = stringSlice(mantissa, 1);
    }
    const dot = stringIndexOf(mantissa, '.');
    const intPart = dot < 0 ? mantissa : stringSlice(mantissa, 0, dot);
    const fracPart = dot < 0 ? '' : stringSlice(mantissa, dot + 1);
    const digits = intPart + fracPart;
    let head = 0;
    while (head + 1 < digits.length && digits[head] === '0') head += 1;
    const clean = stringSlice(digits, head);
    const point = intPart.length - (digits.length - clean.length) + exponent;
    if (point <= 0) return sign + '0.' + stringRepeat('0', -point) + clean;
    if (point >= clean.length) return sign + clean + stringRepeat('0', point - clean.length);
    return sign + stringSlice(clean, 0, point) + '.' + stringSlice(clean, point);
  }

  // https://402.ecma-international.org/#sec-tointlmathematicalvalue
  function intlMathematicalValue(value) {
    const primitive = primitiveNumberHint(value);
    if (typeof primitive === 'bigint') {
      return { number: 0, exact: stringConstructor(primitive) };
    }
    if (typeof primitive === 'string') {
      const text = stringTrim(primitive);
      if (regexpTest(/^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)$/, text)) {
        return { number: 0, exact: text };
      }
      const expanded = expandDecimalLiteral(text);
      if (expanded !== null) {
        return { number: 0, exact: expanded };
      }
    }
    return { number: +primitive, exact: '' };
  }

  // https://402.ecma-international.org/#sec-intl.numberformat
  function NumberFormat(locales, options) {
    if (!new.target) return new NumberFormat(locales, options);
    const locale = resolveLocale(locales);
    options = optionsObject(options);
    stringOption(options, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
    const numberingSystemOption = options.numberingSystem;
    let numberingSystem = 'latn';
    if (numberingSystemOption !== undefined) {
      numberingSystem = stringToLowerCase(stringConstructor(numberingSystemOption));
      if (!regexpTest(/^[a-z0-9]{3,8}(?:-[a-z0-9]{3,8})*$/, numberingSystem)) {
        throw new RangeError('invalid numberingSystem');
      }
      if (numberingSystem !== 'latn') {
        throw new RangeError('only the latn numbering system is currently supported');
      }
    }

    const style = stringOption(
      options, 'style', ['decimal', 'percent', 'currency', 'unit'], 'decimal'
    );
    const currencyOption = options.currency;
    let currency;
    if (currencyOption !== undefined) {
      const currencyText = stringConstructor(currencyOption);
      if (!regexpTest(/^[A-Za-z]{3}$/, currencyText)) throw new RangeError('invalid currency');
      currency = stringToUpperCase(currencyText);
    }
    if (style === 'currency' && currency === undefined) {
      throw new TypeError('currency is required');
    }
    const currencyDisplay = stringOption(
      options, 'currencyDisplay', ['symbol', 'narrowSymbol', 'code', 'name'], 'symbol'
    );
    const currencySign = stringOption(
      options, 'currencySign', ['standard', 'accounting'], 'standard'
    );
    const unitOption = options.unit;
    const unit = unitOption === undefined ? undefined : stringConstructor(unitOption);
    if (style === 'unit' && unit === undefined) throw new TypeError('unit is required');
    stringOption(options, 'unitDisplay', ['short', 'narrow', 'long'], 'short');
    if (style === 'unit') throw new RangeError('unit formatting is not currently supported');

    const notation = stringOption(
      options, 'notation', ['standard', 'scientific', 'engineering', 'compact'], 'standard'
    );
    if (notation !== 'standard') throw new RangeError('notation is not currently supported');
    const currencyDigits = style === 'currency'
      ? nativeCurrencyDigits(currency)
      : 0;
    const defaultMaximum = style === 'percent' ? 0 : style === 'currency' ? currencyDigits : 3;
    const defaultMinimum = style === 'currency' ? defaultMaximum : 0;
    const minimumIntegerDigits = digitOption(options, 'minimumIntegerDigits', 1, 1, 21);
    const minimumFractionOption = options.minimumFractionDigits;
    const maximumFractionOption = options.maximumFractionDigits;
    const minimumSignificantOption = options.minimumSignificantDigits;
    const maximumSignificantOption = options.maximumSignificantDigits;
    const roundingIncrement = digitOption(options, 'roundingIncrement', 1, 1, 5000);
    if (roundingIncrement !== 1) {
      throw new RangeError('roundingIncrement is not currently supported');
    }
    const roundingMode = stringOption(
      options, 'roundingMode',
      ['ceil', 'floor', 'expand', 'trunc', 'halfCeil', 'halfFloor', 'halfExpand',
       'halfTrunc', 'halfEven'],
      'halfExpand'
    );
    if (roundingMode !== 'halfExpand') {
      throw new RangeError('roundingMode is not currently supported');
    }
    const roundingPriority = stringOption(
      options, 'roundingPriority', ['auto', 'morePrecision', 'lessPrecision'], 'auto'
    );
    if (roundingPriority !== 'auto') {
      throw new RangeError('roundingPriority is not currently supported');
    }
    const trailingZeroDisplay = stringOption(
      options, 'trailingZeroDisplay', ['auto', 'stripIfInteger'], 'auto'
    );
    if (trailingZeroDisplay !== 'auto') {
      throw new RangeError('trailingZeroDisplay is not currently supported');
    }

    let minimumFractionDigits = 0;
    let maximumFractionDigits = 0;
    let minimumSignificantDigits = 0;
    let maximumSignificantDigits = 0;
    if (minimumSignificantOption !== undefined || maximumSignificantOption !== undefined) {
      minimumSignificantDigits = minimumSignificantOption === undefined
        ? 1
        : digitOption(
          { minimumSignificantDigits: minimumSignificantOption },
          'minimumSignificantDigits', 1, 1, 21
        );
      maximumSignificantDigits = maximumSignificantOption === undefined
        ? 21
        : digitOption(
          { maximumSignificantDigits: maximumSignificantOption },
          'maximumSignificantDigits', 21, minimumSignificantDigits, 21
        );
    } else {
      const specifiedMinimum = minimumFractionOption === undefined
        ? undefined
        : digitOption(
          { minimumFractionDigits: minimumFractionOption },
          'minimumFractionDigits', defaultMinimum, 0, 100
        );
      const specifiedMaximum = maximumFractionOption === undefined
        ? undefined
        : digitOption(
          { maximumFractionDigits: maximumFractionOption },
          'maximumFractionDigits', defaultMaximum, 0, 100
        );
      minimumFractionDigits = specifiedMinimum === undefined
        ? specifiedMaximum === undefined ? defaultMinimum : mathMin(defaultMinimum, specifiedMaximum)
        : specifiedMinimum;
      maximumFractionDigits = specifiedMaximum === undefined
        ? mathMax(defaultMaximum, minimumFractionDigits)
        : specifiedMaximum;
      if (minimumFractionDigits > maximumFractionDigits) {
        throw new RangeError('minimumFractionDigits exceeds maximumFractionDigits');
      }
    }

    stringOption(options, 'compactDisplay', ['short', 'long'], 'short');
    const groupingOption = options.useGrouping;
    let useGrouping = true;
    if (typeof groupingOption === 'string') {
      const grouping = stringOption(
        { useGrouping: groupingOption }, 'useGrouping',
        ['min2', 'auto', 'always', 'true', 'false'], 'auto'
      );
      if (grouping === 'min2') throw new RangeError('min2 grouping is not currently supported');
      useGrouping = grouping !== 'false';
    } else if (groupingOption !== undefined) {
      useGrouping = booleanConstructor(groupingOption);
    }
    if (style !== 'decimal' && !useGrouping) {
      throw new RangeError('disabling grouping is currently supported only for decimal style');
    }
    const signDisplay = stringOption(
      options, 'signDisplay', ['auto', 'never', 'always', 'exceptZero', 'negative'], 'auto'
    );
    // https://402.ecma-international.org/#sec-setnumberformatunitoptions
    const slots = {
      locale, numberingSystem, style,
      currency: style === 'currency' ? currency : undefined,
      currencyDisplay, currencySign,
      minimumIntegerDigits, minimumFractionDigits, maximumFractionDigits,
      minimumSignificantDigits, maximumSignificantDigits,
      useGrouping, signDisplay, boundFormat: null,
    };
    numberSlotsSet(this, slots);
  }
  Object.defineProperty(NumberFormat, 'length', { value: 0 });

  Object.defineProperty(NumberFormat.prototype, 'format', {
    configurable: true,
    get: function() {
      const slots = numberSlotsGet(this);
      if (!slots) throw new TypeError('incompatible NumberFormat receiver');
      if (!slots.boundFormat) {
        slots.boundFormat = value => {
          const mathematical = intlMathematicalValue(value);
          return nativeFormatNumber([
            slots.locale,
            mathematical.number,
            mathematical.exact,
            slots.style,
            slots.currency || '',
            slots.currencyDisplay,
            slots.currencySign,
            slots.minimumIntegerDigits,
            slots.minimumFractionDigits,
            slots.maximumFractionDigits,
            slots.minimumSignificantDigits,
            slots.maximumSignificantDigits,
            slots.useGrouping,
            slots.signDisplay
          ]);
        };
      }
      return slots.boundFormat;
    },
  });
  Object.defineProperty(
    Object.getOwnPropertyDescriptor(NumberFormat.prototype, 'format').get,
    'name',
    { value: 'get format' }
  );

  Object.defineProperty(NumberFormat.prototype, 'resolvedOptions', {
    value: function resolvedOptions() {
      const slots = numberSlotsGet(this);
      if (!slots) throw new TypeError('incompatible NumberFormat receiver');
      const result = {
        locale: slots.locale,
        numberingSystem: slots.numberingSystem,
        style: slots.style,
      };
      if (slots.currency) {
        result.currency = slots.currency;
        result.currencyDisplay = slots.currencyDisplay;
        result.currencySign = slots.currencySign;
      }
      result.minimumIntegerDigits = slots.minimumIntegerDigits;
      if (slots.maximumSignificantDigits) {
        result.minimumSignificantDigits = slots.minimumSignificantDigits;
        result.maximumSignificantDigits = slots.maximumSignificantDigits;
      } else {
        result.minimumFractionDigits = slots.minimumFractionDigits;
        result.maximumFractionDigits = slots.maximumFractionDigits;
      }
      result.useGrouping = slots.useGrouping ? 'auto' : false;
      result.notation = 'standard';
      result.signDisplay = slots.signDisplay;
      result.roundingIncrement = 1;
      result.roundingMode = 'halfExpand';
      result.roundingPriority = 'auto';
      result.trailingZeroDisplay = 'auto';
      return result;
    },
    writable: true,
    configurable: true,
  });
  Object.defineProperty(NumberFormat, 'supportedLocalesOf', {
    value: function supportedLocalesOf(locales) {
      return supportedLocales(locales, arguments[1]);
    },
    writable: true,
    configurable: true,
  });
  Object.defineProperty(NumberFormat.prototype, Symbol.toStringTag, {
    value: 'Intl.NumberFormat',
    configurable: true,
  });
  Object.defineProperty(NumberFormat, 'prototype', { writable: false });

  const styleCode = { short: 1, medium: 2, long: 3, full: 4 };
  const dateComponentNames = ['weekday', 'era', 'year', 'month', 'day'];
  const timeComponentNames = [
    'dayPeriod', 'hour', 'minute', 'second', 'fractionalSecondDigits', 'timeZoneName'
  ];
  const allComponentNames = arrayConcat(dateComponentNames, timeComponentNames);

  // https://402.ecma-international.org/#sec-todatetimeoptions
  function toDateTimeOptions(options) {
    return objectCreate(options === undefined ? null : optionsObject(options));
  }

  const shortMonths = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  const longMonths = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
  const shortWeekdays = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
  const longWeekdays = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
  const dateGetUTCDay = Date.prototype.getUTCDay;

  function pad2(n) {
    return n < 10 ? '0' + n : '' + n;
  }

  function datePatternCode(components) {
    const hasYear = components.year !== undefined;
    const hasMonth = components.month !== undefined;
    const hasDay = components.day !== undefined;
    const hasWeekday = components.weekday !== undefined;
    if (components.era !== undefined) {
      return -1;
    }
    if (!hasYear && !hasMonth && !hasDay && !hasWeekday) return 0;
    if (hasYear && hasMonth && hasDay) {
      if (hasWeekday) return 4;
      if (components.month === 'long') return 3;
      if (components.month === 'short' || components.month === 'narrow') return 2;
      if (components.year === '2-digit') return 1;
      return 5;
    }
    return -1;
  }

  function formatCustomDateTime(date, slots) {
    const c = slots.components;
    const year = call(dateGetUTCFullYear, date);
    const month = call(dateGetUTCMonth, date);
    const day = call(dateGetUTCDate, date);
    const weekday = call(dateGetUTCDay, date);
    const hour = call(dateGetUTCHours, date);
    const minute = call(dateGetUTCMinutes, date);
    const second = call(dateGetUTCSeconds, date);

    const dateParts = [];
    if (c.weekday !== undefined) {
      const w = c.weekday === 'long' ? longWeekdays[weekday] : c.weekday === 'narrow' ? shortWeekdays[weekday][0] : shortWeekdays[weekday];
      arrayPush(dateParts, w);
    }
    if (c.month !== undefined || c.day !== undefined) {
      let md = '';
      if (c.month !== undefined) {
        if (c.month === 'long') md += longMonths[month];
        else if (c.month === 'short') md += shortMonths[month];
        else if (c.month === 'narrow') md += shortMonths[month][0];
        else if (c.month === '2-digit') md += pad2(month + 1);
        else md += '' + (month + 1);
      }
      if (c.day !== undefined) {
        const dStr = c.day === '2-digit' ? pad2(day) : '' + day;
        if (md) {
          md += (c.month === 'numeric' || c.month === '2-digit') ? '/' + dStr : ' ' + dStr;
        } else {
          md = dStr;
        }
      }
      arrayPush(dateParts, md);
    }
    if (c.year !== undefined) {
      const yStr = c.year === '2-digit' ? pad2(Math.abs(year) % 100) : '' + year;
      arrayPush(dateParts, yStr);
    }
    if (c.era !== undefined) {
      const era = year <= 0 ? (c.era === 'long' ? 'Before Christ' : c.era === 'narrow' ? 'B' : 'BC') : (c.era === 'long' ? 'Anno Domini' : c.era === 'narrow' ? 'A' : 'AD');
      arrayPush(dateParts, era);
    }
    let out = dateParts.join(', ');
    if (c.hour !== undefined || c.minute !== undefined || c.second !== undefined) {
      const timeParts = [];
      let suffix = '';
      if (c.hour !== undefined) {
        let h = hour;
        if (slots.hour12) {
          suffix = h >= 12 ? ' PM' : ' AM';
          h = h % 12 || 12;
        }
        arrayPush(timeParts, c.hour === '2-digit' ? pad2(h) : '' + h);
      }
      if (c.minute !== undefined) arrayPush(timeParts, c.minute === '2-digit' || c.hour !== undefined ? pad2(minute) : '' + minute);
      if (c.second !== undefined) arrayPush(timeParts, c.second === '2-digit' || c.minute !== undefined ? pad2(second) : '' + second);
      const timeStr = timeParts.join(':') + suffix;
      out = out ? out + ', ' + timeStr : timeStr;
    }
    if (c.timeZoneName !== undefined) {
      out = out ? out + ' ' + slots.timeZone : slots.timeZone;
    }
    return out;
  }

  // https://402.ecma-international.org/#sec-createdatetimeformat
  function initializeDateTimeFormat(instance, locales, options, required, defaults) {
    const locale = resolveLocale(locales);
    options = toDateTimeOptions(options);
    stringOption(options, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
    const calendarOption = options.calendar;
    if (calendarOption !== undefined &&
        stringToLowerCase(stringConstructor(calendarOption)) !== 'gregory') {
      throw new RangeError('only the gregory calendar is currently supported');
    }
    const numberingSystemOption = options.numberingSystem;
    if (numberingSystemOption !== undefined &&
        stringToLowerCase(stringConstructor(numberingSystemOption)) !== 'latn') {
      throw new RangeError('only the latn numbering system is currently supported');
    }
    const hour12Option = options.hour12;
    if (hour12Option !== undefined) booleanConstructor(hour12Option);
    const hourCycleOption = stringOption(
      options, 'hourCycle', ['h11', 'h12', 'h23', 'h24'], undefined
    );
    if (hour12Option !== undefined || hourCycleOption !== undefined) {
      throw new RangeError('hour cycle overrides are not currently supported');
    }
    const timeZoneOption = options.timeZone;
    const timeZone = timeZoneOption === undefined ? 'UTC' : stringConstructor(timeZoneOption);
    if (stringToUpperCase(timeZone) !== 'UTC') {
      throw new RangeError('only UTC is currently supported');
    }

    const components = {
      weekday: stringOption(options, 'weekday', ['narrow', 'short', 'long'], undefined),
      era: stringOption(options, 'era', ['narrow', 'short', 'long'], undefined),
      year: stringOption(options, 'year', ['2-digit', 'numeric'], undefined),
      month: stringOption(
        options, 'month', ['2-digit', 'numeric', 'narrow', 'short', 'long'], undefined
      ),
      day: stringOption(options, 'day', ['2-digit', 'numeric'], undefined),
      dayPeriod: stringOption(options, 'dayPeriod', ['narrow', 'short', 'long'], undefined),
      hour: stringOption(options, 'hour', ['2-digit', 'numeric'], undefined),
      minute: stringOption(options, 'minute', ['2-digit', 'numeric'], undefined),
      second: stringOption(options, 'second', ['2-digit', 'numeric'], undefined),
      fractionalSecondDigits: options.fractionalSecondDigits === undefined
        ? undefined
        : digitOption(options, 'fractionalSecondDigits', undefined, 1, 3),
      timeZoneName: stringOption(
        options, 'timeZoneName',
        ['short', 'long', 'shortOffset', 'longOffset', 'shortGeneric', 'longGeneric'],
        undefined
      ),
    };
    stringOption(options, 'formatMatcher', ['basic', 'best fit'], 'best fit');
    const dateStyle = stringOption(
      options, 'dateStyle', ['full', 'long', 'medium', 'short'], undefined
    );
    const timeStyle = stringOption(
      options, 'timeStyle', ['full', 'long', 'medium', 'short'], undefined
    );
    if (dateStyle !== undefined || timeStyle !== undefined) {
      for (let index = 0; index < allComponentNames.length; index += 1) {
        const name = allComponentNames[index];
        if (components[name] !== undefined) {
          throw new TypeError('dateStyle/timeStyle cannot be combined with components');
        }
      }
      if (required === 'date' && timeStyle !== undefined) {
        throw new TypeError('timeStyle is not valid for date-only formatting');
      }
      if (required === 'time' && dateStyle !== undefined) {
        throw new TypeError('dateStyle is not valid for time-only formatting');
      }
    }

    let hasTimeComponents = false;
    let hasDateComponents = false;
    for (let index = 0; index < dateComponentNames.length; index += 1) {
      if (components[dateComponentNames[index]] !== undefined) hasDateComponents = true;
    }
    for (let index = 0; index < timeComponentNames.length; index += 1) {
      if (components[timeComponentNames[index]] !== undefined) hasTimeComponents = true;
    }
    // https://402.ecma-international.org/#sec-createdatetimeformat
    const needDefaults = dateStyle === undefined && timeStyle === undefined &&
      (required === 'date'
        ? !hasDateComponents
        : required === 'time'
          ? !hasTimeComponents
          : !hasDateComponents && !hasTimeComponents);
    if (needDefaults) {
      if (defaults === 'date' || defaults === 'all') {
        components.year = components.month = components.day = 'numeric';
      }
      if (defaults === 'time' || defaults === 'all') {
        components.hour = components.minute = components.second = 'numeric';
        hasTimeComponents = true;
      }
    }
    let customFormat = false;
    if (hasTimeComponents && (components.hour === undefined || components.minute === undefined)) {
      customFormat = true;
    }
    if (components.dayPeriod !== undefined || components.fractionalSecondDigits !== undefined ||
        components.timeZoneName !== undefined) {
      customFormat = true;
    }
    const dateCode = dateStyle === undefined
      ? datePatternCode(components)
      : styleCode[dateStyle];
    if (dateCode < 0) {
      customFormat = true;
    }
    const timeCode = timeStyle === undefined
      ? hasTimeComponents ? (components.second === undefined ? 1 : 2) : 0
      : styleCode[timeStyle];
    const usesHour = timeCode !== 0 || components.hour !== undefined;
    const hour12 = locale === 'en-US' || locale === 'ko-KR';
    dateTimeSlotsSet(instance, {
      locale, dateStyle, timeStyle, timeZone: 'UTC', components,
      dateCode: dateCode < 0 ? 0 : dateCode, timeCode, customFormat, usesHour, hour12, boundFormat: null,
    });
  }

  // https://402.ecma-international.org/#sec-intl.datetimeformat
  function DateTimeFormat(locales, options) {
    if (!new.target) return new DateTimeFormat(locales, options);
    initializeDateTimeFormat(this, locales, options, 'any', 'date');
  }
  Object.defineProperty(DateTimeFormat, 'length', { value: 0 });

  Object.defineProperty(DateTimeFormat.prototype, 'format', {
    configurable: true,
    get: function() {
      const slots = dateTimeSlotsGet(this);
      if (!slots) throw new TypeError('incompatible DateTimeFormat receiver');
      if (!slots.boundFormat) {
        slots.boundFormat = value => {
          if (slots.timeStyle === 'long' || slots.timeStyle === 'full') {
            throw new RangeError('long time styles require unsupported time zone names');
          }
          const date = new dateConstructor(
            value === undefined ? dateNow() : +value
          );
          if (!numberIsFinite(call(dateValueOf, date))) throw new RangeError('invalid time value');
          if (slots.customFormat) {
            return formatCustomDateTime(date, slots);
          }
          return nativeFormatDateTime([
            slots.locale,
            call(dateGetUTCFullYear, date), call(dateGetUTCMonth, date) + 1,
            call(dateGetUTCDate, date), call(dateGetUTCHours, date),
            call(dateGetUTCMinutes, date), call(dateGetUTCSeconds, date),
            slots.dateCode, slots.timeCode
          ]);
        };
      }
      return slots.boundFormat;
    },
  });
  Object.defineProperty(
    Object.getOwnPropertyDescriptor(DateTimeFormat.prototype, 'format').get,
    'name',
    { value: 'get format' }
  );

  Object.defineProperty(DateTimeFormat.prototype, 'resolvedOptions', {
    value: function resolvedOptions() {
      const slots = dateTimeSlotsGet(this);
      if (!slots) throw new TypeError('incompatible DateTimeFormat receiver');
      const result = {
        locale: slots.locale,
        calendar: 'gregory',
        numberingSystem: 'latn',
        timeZone: slots.timeZone,
      };
      if (slots.usesHour) {
        result.hourCycle = slots.hour12 ? 'h12' : 'h23';
        result.hour12 = slots.hour12;
      }
      if (slots.dateStyle) result.dateStyle = slots.dateStyle;
      if (slots.timeStyle) result.timeStyle = slots.timeStyle;
      if (!slots.dateStyle && !slots.timeStyle) {
        for (let index = 0; index < allComponentNames.length; index += 1) {
          const name = allComponentNames[index];
          if (slots.components[name] !== undefined) result[name] = slots.components[name];
        }
      }
      return result;
    },
    writable: true,
    configurable: true,
  });
  Object.defineProperty(DateTimeFormat, 'supportedLocalesOf', {
    value: function supportedLocalesOf(locales) {
      return supportedLocales(locales, arguments[1]);
    },
    writable: true,
    configurable: true,
  });
  Object.defineProperty(DateTimeFormat.prototype, Symbol.toStringTag, {
    value: 'Intl.DateTimeFormat',
    configurable: true,
  });
  Object.defineProperty(DateTimeFormat, 'prototype', { writable: false });

  // https://tc39.es/ecma402/#segmenter-objects
  const graphemePattern = /\r\n|\p{RI}\p{RI}|\p{Extended_Pictographic}(?:\p{Emoji_Modifier}|\uFE0F\u20E3?|[\u{E0020}-\u{E007E}]+\u{E007F})?(?:\u200D\p{Extended_Pictographic}(?:\p{Emoji_Modifier}|\uFE0F\u20E3?|[\u{E0020}-\u{E007E}]+\u{E007F})?)*|.\p{M}*|[\r\n]/gu;
  const wordPattern = /[\p{L}\p{N}_]+(?:['’.-][\p{L}\p{N}_]+)*|\s+|./gus;
  const sentencePattern = /[^.!?]+(?:[.!?]+["')\]}\u201D\u2019]*\s*|$)/gus;
  const wordLikePattern = /[\p{L}\p{N}]/u;

  function computeSegments(input, granularity) {
    const out = [];
    if (input.length === 0) return out;
    const re = granularity === 'word'
      ? wordPattern
      : granularity === 'sentence'
        ? sentencePattern
        : graphemePattern;
    re.lastIndex = 0;
    let match;
    while ((match = re.exec(input)) !== null) {
      const segment = match[0];
      if (segment.length === 0) {
        re.lastIndex += 1;
        continue;
      }
      const entry = { segment, index: match.index, input };
      if (granularity === 'word') {
        entry.isWordLike = regexpTest(wordLikePattern, segment);
      }
      arrayPush(out, entry);
    }
    return out;
  }

  const iteratorPrototype = Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]()));
  const SegmentIteratorPrototype = objectCreate(iteratorPrototype);
  Object.defineProperty(SegmentIteratorPrototype, Symbol.toStringTag, {
    value: 'Segmenter String Iterator', configurable: true,
  });
  Object.defineProperty(SegmentIteratorPrototype, 'next', {
    value: function next() {
      const state = segmentIterSlotsGet(this);
      if (state === undefined) throw new TypeError('incompatible Segmenter String Iterator receiver');
      if (state.cursor >= state.list.length) {
        return { value: undefined, done: true };
      }
      const value = state.list[state.cursor];
      state.cursor += 1;
      return { value, done: false };
    },
    writable: true,
    configurable: true,
  });

  const SegmentsPrototype = {};
  Object.defineProperties(SegmentsPrototype, {
    containing: {
      value: function containing(index) {
        const state = segmentsSlotsGet(this);
        if (state === undefined) throw new TypeError('incompatible Segments receiver');
        const numeric = +index;
        const n = Number.isNaN(numeric) || numeric === 0 ? 0 : numberIsFinite(numeric) ? (numeric < 0 ? Math.ceil(numeric) : mathFloor(numeric)) : numeric;
        if (n < 0 || n >= state.input.length) return undefined;
        const list = computeSegments(state.input, state.granularity);
        for (let i = 0; i < list.length; i += 1) {
          const item = list[i];
          if (n >= item.index && n < item.index + item.segment.length) {
            return item;
          }
        }
        return undefined;
      },
      writable: true,
      configurable: true,
    },
    [Symbol.iterator]: {
      value: function iterator() {
        const state = segmentsSlotsGet(this);
        if (state === undefined) throw new TypeError('incompatible Segments receiver');
        const iter = objectCreate(SegmentIteratorPrototype);
        segmentIterSlotsSet(iter, {
          list: computeSegments(state.input, state.granularity),
          cursor: 0,
        });
        return iter;
      },
      writable: true,
      configurable: true,
    },
  });

  // https://tc39.es/ecma402/#sec-intl.segmenter
  function Segmenter(locales, options) {
    if (!new.target) throw new TypeError("Constructor Intl.Segmenter requires 'new'");
    const locale = resolveLocale(locales);
    options = optionsObject(options);
    stringOption(options, 'localeMatcher', ['lookup', 'best fit'], 'best fit');
    const granularity = stringOption(
      options, 'granularity', ['grapheme', 'word', 'sentence'], 'grapheme'
    );
    segmenterSlotsSet(this, { locale, granularity });
  }

  Object.defineProperty(Segmenter, 'supportedLocalesOf', {
    value: function supportedLocalesOf(locales, options) {
      return supportedLocales(locales, options);
    },
    writable: true,
    configurable: true,
  });

  Object.defineProperties(Segmenter.prototype, {
    segment: {
      value: function segment(string) {
        const state = segmenterSlotsGet(this);
        if (state === undefined) throw new TypeError('incompatible Intl.Segmenter receiver');
        const input = stringConstructor(string);
        const segments = objectCreate(SegmentsPrototype);
        segmentsSlotsSet(segments, { input, granularity: state.granularity });
        return segments;
      },
      writable: true,
      configurable: true,
    },
    resolvedOptions: {
      value: function resolvedOptions() {
        const state = segmenterSlotsGet(this);
        if (state === undefined) throw new TypeError('incompatible Intl.Segmenter receiver');
        return { locale: state.locale, granularity: state.granularity };
      },
      writable: true,
      configurable: true,
    },
    [Symbol.toStringTag]: { value: 'Intl.Segmenter', configurable: true },
  });

  const IntlObject = {};
  Object.defineProperties(IntlObject, {
    NumberFormat: { value: NumberFormat, writable: true, configurable: true },
    DateTimeFormat: { value: DateTimeFormat, writable: true, configurable: true },
    Segmenter: { value: Segmenter, writable: true, configurable: true },
    getCanonicalLocales: {
      // https://402.ecma-international.org/#sec-intl.getcanonicallocales
      value: function getCanonicalLocales(locales) {
        return canonicalLocaleList(locales);
      },
      writable: true,
      configurable: true,
    },
  });
  Object.defineProperty(IntlObject, Symbol.toStringTag, {
    value: 'Intl', configurable: true,
  });
  Object.defineProperty(globalThis, 'Intl', {
    value: IntlObject,
    writable: true,
    configurable: true,
  });

  // https://402.ecma-international.org/#sup-number.prototype.tolocalestring
  // https://402.ecma-international.org/#sup-date.prototype.tolocalestring
  function localeMethod(name, compute) {
    const method = function() { return compute(this, arguments[0], arguments[1]); };
    Object.defineProperty(method, 'name', { value: name, configurable: true });
    return method;
  }

  // https://searchfox.org/firefox-main/source/js/src/builtin/intl/NumberFormat.cpp#2023
  function numberLocaleMethod(unbox) {
    // https://402.ecma-international.org/#sec-formatnumeric
    return localeMethod('toLocaleString', (receiver, locales, options) => {
      const value = call(unbox, receiver);
      return new NumberFormat(locales, options).format(value);
    });
  }

  function dateLocaleMethod(name, dateStyle, timeStyle) {
    return localeMethod(name, (receiver, locales, options) => {
      const value = call(dateValueOf, receiver);
      if (!numberIsFinite(value)) return 'Invalid Date';
      const formatter = objectCreate(DateTimeFormat.prototype);
      initializeDateTimeFormat(formatter, locales, options, dateStyle, timeStyle);
      return formatter.format(value);
    });
  }

  Object.defineProperty(Number.prototype, 'toLocaleString', {
    value: numberLocaleMethod(numberValueOf), writable: true, configurable: true,
  });
  Object.defineProperty(BigInt.prototype, 'toLocaleString', {
    value: numberLocaleMethod(bigintValueOf), writable: true, configurable: true,
  });
  Object.defineProperties(Date.prototype, {
    toLocaleString: {
      value: dateLocaleMethod('toLocaleString', 'any', 'all'),
      writable: true, configurable: true,
    },
    toLocaleDateString: {
      value: dateLocaleMethod('toLocaleDateString', 'date', 'date'),
      writable: true, configurable: true,
    },
    toLocaleTimeString: {
      value: dateLocaleMethod('toLocaleTimeString', 'time', 'time'),
      writable: true, configurable: true,
    },
  });

})();
