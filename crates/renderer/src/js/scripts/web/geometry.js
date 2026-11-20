// https://drafts.fxtf.org/geometry/#DOMMatrix
(function () {
  if (typeof globalThis.DOMMatrix === 'function') {
    return;
  }

  function identity() {
    return [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
  }

  function values(init) {
    var matrix = identity();
    if (Array.isArray(init)) {
      if (init.length === 6) {
        matrix[0] = Number(init[0]);
        matrix[1] = Number(init[1]);
        matrix[4] = Number(init[2]);
        matrix[5] = Number(init[3]);
        matrix[12] = Number(init[4]);
        matrix[13] = Number(init[5]);
        return matrix;
      }
      if (init.length !== 16) {
        throw new TypeError('DOMMatrix sequence must have 6 or 16 elements');
      }
      for (var index = 0; index < 16; index += 1) {
        matrix[index] = Number(init[index]);
      }
      return matrix;
    }
    if (init === undefined || init === null) {
      return matrix;
    }
    if (typeof init !== 'object') {
      throw new TypeError('DOMMatrixInit must be an object');
    }
    var twoD = { a: 0, b: 1, c: 4, d: 5, e: 12, f: 13 };
    for (var name in twoD) {
      if (name in init) matrix[twoD[name]] = Number(init[name]);
    }
    for (var row = 1; row <= 4; row += 1) {
      for (var column = 1; column <= 4; column += 1) {
        var key = 'm' + row + column;
        if (key in init) matrix[(row - 1) * 4 + column - 1] = Number(init[key]);
      }
    }
    return matrix;
  }

  function DOMMatrix(init) {
    if (!(this instanceof DOMMatrix)) {
      throw new TypeError('DOMMatrix must be constructed with new');
    }
    Object.defineProperty(this, '_m', {
      configurable: true,
      enumerable: false,
      writable: true,
      value: values(init),
    });
  }

  function accessor(name, index) {
    Object.defineProperty(DOMMatrix.prototype, name, {
      configurable: true,
      enumerable: true,
      get: function () { return this._m[index]; },
      set: function (value) { this._m[index] = Number(value); },
    });
  }

  accessor('a', 0);
  accessor('b', 1);
  accessor('c', 4);
  accessor('d', 5);
  accessor('e', 12);
  accessor('f', 13);
  for (var row = 1; row <= 4; row += 1) {
    for (var column = 1; column <= 4; column += 1) {
      accessor('m' + row + column, (row - 1) * 4 + column - 1);
    }
  }
  Object.defineProperty(DOMMatrix.prototype, 'is2D', {
    configurable: true,
    enumerable: true,
    get: function () { return true; },
  });
  Object.defineProperty(DOMMatrix.prototype, 'isIdentity', {
    configurable: true,
    enumerable: true,
    get: function () {
      var identityMatrix = identity();
      for (var index = 0; index < 16; index += 1) {
        if (this._m[index] !== identityMatrix[index]) return false;
      }
      return true;
    },
  });

  DOMMatrix.fromMatrix = function (init) { return new DOMMatrix(init); };
  DOMMatrix.fromFloat32Array = function (array) { return new DOMMatrix(array); };
  DOMMatrix.fromFloat64Array = function (array) { return new DOMMatrix(array); };

  globalThis.DOMMatrix = DOMMatrix;
})();
