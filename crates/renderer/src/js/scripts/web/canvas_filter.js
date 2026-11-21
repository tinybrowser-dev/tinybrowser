// https://html.spec.whatwg.org/multipage/canvas.html#canvasfilter
(function () {
  function fail() {
    throw new TypeError('invalid filter');
  }

  function number(value) {
    if (value !== null && typeof value === 'object' && !Array.isArray(value)) fail();
    if (typeof value === 'undefined') fail();
    var result = Number(value);
    if (!Number.isFinite(result)) fail();
    return result;
  }

  function nonNegative(value) {
    var result = number(value);
    if (result < 0) fail();
    return result;
  }

  function pair(value, maximum) {
    if (Array.isArray(value)) {
      if (value.length > maximum) fail();
      return value.map(number);
    }
    return [number(value)];
  }

  function nonNegativePair(value, maximum) {
    if (Array.isArray(value)) {
      if (value.length > maximum) fail();
      return value.map(nonNegative);
    }
    return [nonNegative(value)];
  }

  function color(value) {
    if (typeof value !== 'string') fail();
    var text = value.trim().toLowerCase();
    if (/^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/.test(text)) return;
    if (/^[a-z]+$/.test(text)) {
      if (text === 'test') fail();
      return;
    }
    if (/^(rgba?|hsla?)\(/.test(text) && !text.includes('nan')) return;
    fail();
  }

  function validate(init) {
    if (!init || typeof init !== 'object') fail();
    var name = init.name;
    if (name === undefined) fail();
    switch (name) {
      case 'gaussianBlur':
        if (!('stdDeviation' in init)) fail();
        pair(init.stdDeviation, 2);
        break;
      case 'turbulence':
        if ('baseFrequency' in init) nonNegativePair(init.baseFrequency, 2);
        if ('numOctaves' in init) nonNegativePair(init.numOctaves, 1);
        if ('seed' in init) pair(init.seed, 1);
        if ('stitchTiles' in init &&
            init.stitchTiles !== 'stitch' && init.stitchTiles !== 'noStitch') {
          fail();
        }
        if ('type' in init &&
            init.type !== 'fractalNoise' && init.type !== 'turbulence') {
          fail();
        }
        break;
      case 'convolveMatrix':
        if (!Array.isArray(init.kernelMatrix) || init.kernelMatrix.length === 0) fail();
        var rows = init.kernelMatrix;
        var widths = rows.map(function (row) {
          if (!Array.isArray(row)) fail();
          row.forEach(function (value) {
            if (!Number.isFinite(Number(value))) fail();
          });
          return row.length;
        });
        var width = widths[0];
        if (width === 0) {
          if (widths.length !== 1) fail();
        } else if (widths.some(function (value) { return value !== width; })) {
          fail();
        }
        break;
      case 'dropShadow':
        if ('dx' in init) pair(init.dx, 1);
        if ('dy' in init) pair(init.dy, 1);
        if ('floodOpacity' in init) pair(init.floodOpacity, 1);
        if ('stdDeviation' in init) pair(init.stdDeviation, 2);
        if ('floodColor' in init) color(init.floodColor);
        break;
      case 'colorMatrix':
        var type = init.type;
        if (type === 'hueRotate' || type === 'saturate') {
          pair(init.values, 1);
        } else if (type === 'luminanceToAlpha') {
          break;
        } else {
          if (!Array.isArray(init.values) || init.values.length !== 20) fail();
          init.values.forEach(function (value) {
            if (!Number.isFinite(Number(value))) fail();
          });
        }
        break;
      default:
        break;
    }
  }

  class CanvasFilter {
    constructor(init) {
      if (Array.isArray(init)) {
        if (init.length === 0) fail();
        init.forEach(validate);
      } else {
        validate(init);
      }
      Object.defineProperty(this, '__tbInit', { value: init });
    }
  }
  Object.defineProperty(CanvasFilter.prototype, Symbol.toStringTag, { value: 'CanvasFilter' });

  globalThis.CanvasFilter = CanvasFilter;
  host.__tbValidateFilter = function (init) {
    if (init === null || init === undefined) return;
    if (Array.isArray(init)) {
      init.forEach(validate);
      return;
    }
    if (typeof init !== 'object') return;
    if (!('name' in init)) return;
    validate(init);
  };
})();
