const __tbBlobData = host.slots('tinybrowser.blob.data');
const __tbFileData = host.slots('tinybrowser.file.data');
const __tbFileListData = host.slots('tinybrowser.filelist.data');
const __tbReaderData = host.slots('tinybrowser.filereader.data');
const __tbProgressData = host.slots('tinybrowser.progress.data');
const __tbDecoderData = host.slots('tinybrowser.textdecoder.data');
const __tbStreamData = host.slots('tinybrowser.readablestream.data');
const __tbToLatin1 = bytes => {
  // https://tc39.es/ecma262/#sec-apply
  const parts = __tbPrivateArray();
  for (let start = 0; start < bytes.length; start += 4096) {
    __tbArray.push(parts, __tbApply(__tbIDLFromCharCode, null, __tbBytesCopy(bytes, start, start + 4096)));
  }
  return __tbArray.join(parts, '');
};
const __tbUtf8Encode = value => host.__tbUtf8Encode(__tbIDLString(value));
const __tbUtf8Decode = bytes => host.__tbDecode(bytes, 'utf-8', false, true);
const __tbEncoding = label => {
  const name = host.__tbEncodingName(String(label));
  return name === null || name === undefined
    ? null
    : String(__tbApply(__tbStringToLowerCase, name, []));
};
const __tbDecodeBytes = (bytes, encoding, fatal, ignoreBOM) =>
  host.__tbDecode(bytes, encoding, fatal, ignoreBOM);
// https://w3c.github.io/FileAPI/#convert-line-endings-to-native
const __tbNativeEndings = value => String(value).replace(/\r\n?|\n/g, '\n');
// https://webidl.spec.whatwg.org/#Clamp
const __tbClampRound = value => {
  value = Number(value);
  if (Number.isNaN(value) || value === 0) return 0;
  if (value === Infinity) return Number.MAX_SAFE_INTEGER;
  if (value === -Infinity) return Number.MIN_SAFE_INTEGER;
  const floor = Math.floor(value);
  const fraction = value - floor;
  if (fraction < 0.5) return floor;
  if (fraction > 0.5) return floor + 1;
  return floor % 2 === 0 ? floor : floor + 1;
};
globalThis.TextEncoder = __tbInstallInterface(class TextEncoder {
  constructor() {}
  get encoding() { return 'utf-8'; }
  encode(input) {
    return __tbUtf8Encode(input);
  }
  encodeInto(source, destination) {
    const length = __tbApply(__tbIDLTypedArrayLength, destination, []);
    const packed = host.__tbEncodeInto(__tbIDLString(source), length);
    const read = packed.read;
    const written = packed.written;
    const bytes = packed.bytes;
    for (let index = 0; index < written; index++) destination[index] = bytes[index];
    return { read, written };
  }
});
const __tbDecoderFinalizer = (() => {
  try {
    if (typeof FinalizationRegistry === 'function') {
      return new FinalizationRegistry(id => {
        try { host.__tbDecoderFree(id); } catch (_) {}
      });
    }
  } catch (_) {}
  return null;
})();
globalThis.TextDecoder = class TextDecoder {
  constructor(label, options) {
    const optionsObject = options === undefined ? {} : Object(options);
    const encoding = label === undefined ? 'utf-8' : __tbEncoding(__tbIDLString(label));
    if (encoding === null) throw new RangeError('The encoding label is not supported');
    const fatal = optionsObject.fatal !== undefined && Boolean(optionsObject.fatal);
    const ignoreBOM = optionsObject.ignoreBOM !== undefined && Boolean(optionsObject.ignoreBOM);
    // https://encoding.spec.whatwg.org/#dom-textdecoder-decode
    const decoder = host.__tbDecoderInit(encoding, ignoreBOM);
    __tbDecoderData.set(this, { decoder, encoding, fatal, ignoreBOM });
    if (__tbDecoderFinalizer !== null) {
      try { __tbDecoderFinalizer.register(this, decoder); } catch (_) {}
    }
  }
  get encoding() { return __tbBrand(this, __tbDecoderData).encoding; }
  get fatal() { return __tbBrand(this, __tbDecoderData).fatal; }
  get ignoreBOM() { return __tbBrand(this, __tbDecoderData).ignoreBOM; }
  decode(input, options) {
    const data = __tbBrand(this, __tbDecoderData);
    let bytes = new __tbUint8Array(0);
    if (input !== undefined && input !== null) {
      if (__tbIsArrayBufferView(input)) {
        const buffer = input.buffer;
        const offset = input.byteOffset;
        const length = input.byteLength;
        bytes = new __tbUint8Array(buffer, offset, length);
      } else if (input instanceof ArrayBuffer) bytes = new __tbUint8Array(input);
      else throw new TypeError('The input argument must be an ArrayBuffer or ArrayBufferView');
    }
    const stream = options !== undefined && options !== null && Boolean(options.stream);
    return host.__tbDecoderDecode(data.decoder, bytes, data.fatal, stream);
  }
};
Object.defineProperty(globalThis.TextDecoder.prototype, Symbol.toStringTag, { value: 'TextDecoder', writable: false, enumerable: false, configurable: true });
// https://html.spec.whatwg.org/multipage/webappapis.html#atob
const __tbBase64Encode = bytes => host.__tbBase64Encode(__tbToLatin1(bytes));
globalThis.btoa = function(input) {
  const encoded = host.__tbBtoa(__tbIDLString(input));
  if (encoded === null || encoded === undefined) {
    throw new DOMException('The string to be encoded contains characters outside of the Latin1 range.', 'InvalidCharacterError');
  }
  return encoded;
};
globalThis.atob = function(input) {
  const decoded = host.__tbAtob(__tbIDLString(input));
  if (decoded === null || decoded === undefined) {
    throw new DOMException('The string to be decoded is not correctly encoded.', 'InvalidCharacterError');
  }
  return decoded;
};
