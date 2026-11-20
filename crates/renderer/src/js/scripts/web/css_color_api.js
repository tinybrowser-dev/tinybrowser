// https://drafts.csswg.org/css-color-5/#color-function
(function () {
  var CSS = globalThis.CSS;
  if (!CSS || typeof CSS !== 'object' || !Object.isExtensible(CSS)) {
    var copy = {};
    if (CSS && typeof CSS === 'object') {
      Object.getOwnPropertyNames(CSS).forEach(function (name) {
        copy[name] = CSS[name];
      });
    }
    CSS = copy;
  }
  if (typeof CSS.percent !== 'function') {
    CSS.percent = function (value) { return { percent: Number(value) }; };
  }
  if (typeof CSS.deg !== 'function') {
    CSS.deg = function (value) { return { deg: Number(value) }; };
  }
  if (typeof CSS.rad !== 'function') {
    CSS.rad = function (value) { return { rad: Number(value) }; };
  }
  if (typeof CSS.num !== 'function') {
    CSS.num = function (value) { return { num: Number(value) }; };
  }
  globalThis.CSS = CSS;

  function number(value) {
    if (value !== null && typeof value === 'object') {
      if ('percent' in value) return Number(value.percent) / 100;
      if ('deg' in value) return Number(value.deg);
      if ('rad' in value) return Number(value.rad) * 180 / Math.PI;
      if ('num' in value) return Number(value.num);
    }
    return Number(value);
  }

  function clamp01(value) {
    return Math.min(1, Math.max(0, number(value)));
  }

  function accessor(prototype, name, get, set) {
    Object.defineProperty(prototype, name, {
      configurable: true,
      enumerable: true,
      get: get,
      set: set,
    });
  }

  function CSSRGB(r, g, b, a) {
    if (!(this instanceof CSSRGB)) return new CSSRGB(r, g, b, a);
    this.r = r;
    this.g = g;
    this.b = b;
    this.alpha = a === undefined ? 1 : a;
  }
  accessor(CSSRGB.prototype, 'r', function () { return this._r; },
           function (value) { this._r = number(value); });
  accessor(CSSRGB.prototype, 'g', function () { return this._g; },
           function (value) { this._g = number(value); });
  accessor(CSSRGB.prototype, 'b', function () { return this._b; },
           function (value) { this._b = number(value); });
  accessor(CSSRGB.prototype, 'alpha', function () { return this._a; },
           function (value) { this._a = clamp01(value); });

  function hslToRgb(h, s, l) {
    h = ((h % 360) + 360) % 360;
    s = Math.min(1, Math.max(0, s));
    l = Math.min(1, Math.max(0, l));
    if (s === 0) return [l, l, l];
    var c = (1 - Math.abs(2 * l - 1)) * s;
    var x = c * (1 - Math.abs(((h / 60) % 2) - 1));
    var m = l - c / 2;
    var rgb;
    if (h < 60) rgb = [c, x, 0];
    else if (h < 120) rgb = [x, c, 0];
    else if (h < 180) rgb = [0, c, x];
    else if (h < 240) rgb = [0, x, c];
    else if (h < 300) rgb = [x, 0, c];
    else rgb = [c, 0, x];
    return [rgb[0] + m, rgb[1] + m, rgb[2] + m];
  }

  function CSSHSL(h, s, l, a) {
    if (!(this instanceof CSSHSL)) return new CSSHSL(h, s, l, a);
    this.h = h;
    this.s = s;
    this.l = l;
    this.alpha = a === undefined ? 1 : a;
  }
  accessor(CSSHSL.prototype, 'h', function () { return this._h; },
           function (value) { this._h = number(value); });
  accessor(CSSHSL.prototype, 's', function () { return this._s; },
           function (value) { this._s = clamp01(value); });
  accessor(CSSHSL.prototype, 'l', function () { return this._l; },
           function (value) { this._l = clamp01(value); });
  accessor(CSSHSL.prototype, 'alpha', function () { return this._a; },
           function (value) { this._a = clamp01(value); });
  accessor(CSSHSL.prototype, 'r',
           function () { return hslToRgb(this._h, this._s, this._l)[0]; });
  accessor(CSSHSL.prototype, 'g',
           function () { return hslToRgb(this._h, this._s, this._l)[1]; });
  accessor(CSSHSL.prototype, 'b',
           function () { return hslToRgb(this._h, this._s, this._l)[2]; });

  globalThis.CSSRGB = CSSRGB;
  globalThis.CSSHSL = CSSHSL;
})();
