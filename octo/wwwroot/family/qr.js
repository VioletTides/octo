/*
 * A small QR code encoder for the Family pages: byte mode, error correction level M,
 * versions 1 to 40, drawn as an inline SVG. Nothing leaves the page to make a code.
 *
 *   OctoQr.matrix(text)        -> { size, version, mask, modules: boolean[][] }  (row, column)
 *   OctoQr.svg(text, label)    -> an <svg> element, dark modules on white with a quiet zone
 */
(function (global) {
  'use strict';

  // Error correction codewords per block and number of blocks, level M, by version (index 0
  // unused).
  var ECC_PER_BLOCK = [-1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26,
    26, 26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28];
  var BLOCKS = [-1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20,
    21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49];
  // The two bits level M writes in the format information.
  var FORMAT_BITS_M = 0;

  function utf8(text) {
    return Array.from(new TextEncoder().encode(text));
  }

  // Modules that carry data in a symbol of this version.
  function rawModules(version) {
    var result = (16 * version + 128) * version + 64;
    if (version >= 2) {
      var align = Math.floor(version / 7) + 2;
      result -= (25 * align - 10) * align - 55;
      if (version >= 7) result -= 36;
    }
    return result;
  }

  function dataCodewords(version) {
    return Math.floor(rawModules(version) / 8) - ECC_PER_BLOCK[version] * BLOCKS[version];
  }

  // Multiplication in GF(2^8) with the QR polynomial x^8 + x^4 + x^3 + x^2 + 1.
  function gfMul(x, y) {
    var z = 0;
    for (var i = 7; i >= 0; i--) {
      z = (z << 1) ^ ((z >>> 7) * 0x11d);
      z ^= ((y >>> i) & 1) * x;
    }
    return z & 0xff;
  }

  function rsDivisor(degree) {
    var result = [];
    for (var i = 0; i < degree - 1; i++) result.push(0);
    result.push(1);
    var root = 1;
    for (var n = 0; n < degree; n++) {
      for (var j = 0; j < result.length; j++) {
        result[j] = gfMul(result[j], root);
        if (j + 1 < result.length) result[j] ^= result[j + 1];
      }
      root = gfMul(root, 0x02);
    }
    return result;
  }

  function rsRemainder(data, divisor) {
    var result = divisor.map(function () { return 0; });
    data.forEach(function (b) {
      var factor = b ^ result.shift();
      result.push(0);
      divisor.forEach(function (coef, i) { result[i] ^= gfMul(coef, factor); });
    });
    return result;
  }

  // The data bits: byte mode, the length, the bytes, a terminator and padding.
  function encodeData(bytes, version) {
    var bits = [];
    function put(value, length) {
      for (var i = length - 1; i >= 0; i--) bits.push((value >>> i) & 1);
    }
    put(4, 4);
    put(bytes.length, version < 10 ? 8 : 16);
    bytes.forEach(function (b) { put(b, 8); });
    var capacity = dataCodewords(version) * 8;
    put(0, Math.min(4, capacity - bits.length));
    put(0, (8 - (bits.length % 8)) % 8);
    var codewords = [];
    for (var i = 0; i < bits.length; i += 8) {
      var b = 0;
      for (var j = 0; j < 8; j++) b = (b << 1) | bits[i + j];
      codewords.push(b);
    }
    for (var pad = 0xec; codewords.length < dataCodewords(version); pad ^= 0xec ^ 0x11) codewords.push(pad);
    return codewords;
  }

  // Splits into blocks, adds each block's error correction and interleaves them.
  function addEcc(data, version) {
    var blocks = BLOCKS[version];
    var eccLen = ECC_PER_BLOCK[version];
    var raw = Math.floor(rawModules(version) / 8);
    var shortBlocks = blocks - (raw % blocks);
    var shortLen = Math.floor(raw / blocks);
    var divisor = rsDivisor(eccLen);
    var all = [];
    for (var i = 0, k = 0; i < blocks; i++) {
      var dat = data.slice(k, k + shortLen - eccLen + (i < shortBlocks ? 0 : 1));
      k += dat.length;
      var ecc = rsRemainder(dat, divisor);
      if (i < shortBlocks) dat.push(0);
      all.push(dat.concat(ecc));
    }
    var result = [];
    for (var n = 0; n < all[0].length; n++) {
      for (var b = 0; b < all.length; b++) {
        if (n !== shortLen - eccLen || b >= shortBlocks) result.push(all[b][n]);
      }
    }
    return result;
  }

  function alignmentPositions(version, size) {
    if (version === 1) return [];
    var count = Math.floor(version / 7) + 2;
    var step = version === 32 ? 26 : Math.ceil((version * 4 + 4) / (count * 2 - 2)) * 2;
    var result = [6];
    for (var pos = size - 7; result.length < count; pos -= step) result.splice(1, 0, pos);
    return result;
  }

  function Symbol(version) {
    this.version = version;
    this.size = version * 4 + 17;
    this.modules = [];
    this.fixed = [];
    for (var y = 0; y < this.size; y++) {
      this.modules.push(new Array(this.size).fill(false));
      this.fixed.push(new Array(this.size).fill(false));
    }
  }

  Symbol.prototype.set = function (x, y, dark) {
    this.modules[y][x] = dark;
    this.fixed[y][x] = true;
  };

  Symbol.prototype.finder = function (x, y) {
    for (var dy = -4; dy <= 4; dy++) {
      for (var dx = -4; dx <= 4; dx++) {
        var xx = x + dx, yy = y + dy;
        if (xx < 0 || yy < 0 || xx >= this.size || yy >= this.size) continue;
        var d = Math.max(Math.abs(dx), Math.abs(dy));
        this.set(xx, yy, d !== 2 && d !== 4);
      }
    }
  };

  Symbol.prototype.alignment = function (x, y) {
    for (var dy = -2; dy <= 2; dy++) {
      for (var dx = -2; dx <= 2; dx++) {
        this.set(x + dx, y + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
      }
    }
  };

  Symbol.prototype.formatBits = function (mask) {
    var data = (FORMAT_BITS_M << 3) | mask;
    var rem = data;
    for (var i = 0; i < 10; i++) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
    var bits = ((data << 10) | rem) ^ 0x5412;
    var bit = function (i) { return ((bits >>> i) & 1) !== 0; };
    var n = this.size;
    for (var a = 0; a <= 5; a++) this.set(8, a, bit(a));
    this.set(8, 7, bit(6));
    this.set(8, 8, bit(7));
    this.set(7, 8, bit(8));
    for (var b = 9; b < 15; b++) this.set(14 - b, 8, bit(b));
    for (var c = 0; c < 8; c++) this.set(n - 1 - c, 8, bit(c));
    for (var d = 8; d < 15; d++) this.set(8, n - 15 + d, bit(d));
    this.set(8, n - 8, true);
  };

  Symbol.prototype.versionBits = function () {
    if (this.version < 7) return;
    var rem = this.version;
    for (var i = 0; i < 12; i++) rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25);
    var bits = (this.version << 12) | rem;
    for (var n = 0; n < 18; n++) {
      var dark = ((bits >>> n) & 1) !== 0;
      var a = this.size - 11 + (n % 3);
      var b = Math.floor(n / 3);
      this.set(a, b, dark);
      this.set(b, a, dark);
    }
  };

  Symbol.prototype.functionPatterns = function () {
    var n = this.size;
    for (var i = 0; i < n; i++) {
      this.set(6, i, i % 2 === 0);
      this.set(i, 6, i % 2 === 0);
    }
    this.finder(3, 3);
    this.finder(n - 4, 3);
    this.finder(3, n - 4);
    var pos = alignmentPositions(this.version, n);
    var last = pos.length - 1;
    for (var a = 0; a < pos.length; a++) {
      for (var b = 0; b < pos.length; b++) {
        var corner = (a === 0 && b === 0) || (a === 0 && b === last) || (a === last && b === 0);
        if (!corner) this.alignment(pos[a], pos[b]);
      }
    }
    this.formatBits(0);
    this.versionBits();
  };

  Symbol.prototype.place = function (codewords) {
    var n = this.size;
    var i = 0;
    var total = codewords.length * 8;
    for (var right = n - 1; right >= 1; right -= 2) {
      if (right === 6) right = 5;
      for (var vert = 0; vert < n; vert++) {
        for (var j = 0; j < 2; j++) {
          var x = right - j;
          var upward = ((right + 1) & 2) === 0;
          var y = upward ? n - 1 - vert : vert;
          if (!this.fixed[y][x] && i < total) {
            this.modules[y][x] = ((codewords[i >>> 3] >>> (7 - (i & 7))) & 1) !== 0;
            i++;
          }
        }
      }
    }
  };

  function masked(mask, x, y) {
    switch (mask) {
      case 0: return (x + y) % 2 === 0;
      case 1: return y % 2 === 0;
      case 2: return x % 3 === 0;
      case 3: return (x + y) % 3 === 0;
      case 4: return (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0;
      case 5: return (x * y) % 2 + (x * y) % 3 === 0;
      case 6: return ((x * y) % 2 + (x * y) % 3) % 2 === 0;
      default: return ((x + y) % 2 + (x * y) % 3) % 2 === 0;
    }
  }

  Symbol.prototype.applyMask = function (mask) {
    for (var y = 0; y < this.size; y++) {
      for (var x = 0; x < this.size; x++) {
        if (!this.fixed[y][x] && masked(mask, x, y)) this.modules[y][x] = !this.modules[y][x];
      }
    }
  };

  // How hard the symbol is to read: long runs, 2x2 blocks, finder-like patterns, and an
  // uneven share of dark modules each cost points. The mask with the fewest points is used.
  Symbol.prototype.penalty = function () {
    var n = this.size, m = this.modules, score = 0, x, y;
    function runs(get) {
      var total = 0;
      for (var a = 0; a < n; a++) {
        var run = 1;
        for (var b = 1; b < n; b++) {
          if (get(a, b) === get(a, b - 1)) {
            run++;
          } else {
            if (run >= 5) total += run - 2;
            run = 1;
          }
        }
        if (run >= 5) total += run - 2;
      }
      return total;
    }
    score += runs(function (a, b) { return m[a][b]; });
    score += runs(function (a, b) { return m[b][a]; });
    for (y = 0; y < n - 1; y++) {
      for (x = 0; x < n - 1; x++) {
        var c = m[y][x];
        if (c === m[y][x + 1] && c === m[y + 1][x] && c === m[y + 1][x + 1]) score += 3;
      }
    }
    var pattern = [true, false, true, true, true, false, true];
    function finderLike(get) {
      var total = 0;
      for (var a = 0; a < n; a++) {
        for (var b = 0; b + 7 <= n; b++) {
          var hit = true;
          for (var k = 0; k < 7 && hit; k++) hit = get(a, b + k) === pattern[k];
          if (!hit) continue;
          var lightBefore = true, lightAfter = true;
          for (var q = 1; q <= 4; q++) {
            if (b - q >= 0 && get(a, b - q)) lightBefore = false;
            if (b + 6 + q < n && get(a, b + 6 + q)) lightAfter = false;
          }
          if (lightBefore || lightAfter) total += 40;
        }
      }
      return total;
    }
    score += finderLike(function (a, b) { return m[a][b]; });
    score += finderLike(function (a, b) { return m[b][a]; });
    var dark = 0;
    for (y = 0; y < n; y++) for (x = 0; x < n; x++) if (m[y][x]) dark++;
    var total = n * n;
    score += Math.floor(Math.abs(dark * 20 - total * 10) / total) * 10;
    return score;
  };

  function build(bytes, version, mask) {
    var symbol = new Symbol(version);
    symbol.functionPatterns();
    symbol.place(addEcc(encodeData(bytes, version), version));
    symbol.applyMask(mask);
    symbol.formatBits(mask);
    return symbol;
  }

  // The smallest version that holds the text, and the best mask (or the ones asked for).
  function matrix(text, options) {
    options = options || {};
    var bytes = utf8(String(text));
    var version = options.version || 0;
    if (!version) {
      for (var v = 1; v <= 40; v++) {
        var headerBits = 4 + (v < 10 ? 8 : 16);
        if (headerBits + bytes.length * 8 <= dataCodewords(v) * 8) {
          version = v;
          break;
        }
      }
    }
    if (!version) throw new Error('Too long for a QR code');
    var best = null, bestMask = 0;
    var masks = options.mask === undefined ? [0, 1, 2, 3, 4, 5, 6, 7] : [options.mask];
    masks.forEach(function (mask) {
      var symbol = build(bytes, version, mask);
      var score = masks.length > 1 ? symbol.penalty() : 0;
      if (best === null || score < best.score) {
        best = { symbol: symbol, score: score };
        bestMask = mask;
      }
    });
    return { size: best.symbol.size, version: version, mask: bestMask, modules: best.symbol.modules };
  }

  var SVG_NS = 'http://www.w3.org/2000/svg';

  // An <svg> of the code: dark modules on white, four modules of quiet zone around it.
  function svg(text, label) {
    var qr = matrix(text);
    var quiet = 4, size = qr.size + quiet * 2;
    var path = '';
    for (var y = 0; y < qr.size; y++) {
      for (var x = 0; x < qr.size; x++) {
        if (qr.modules[y][x]) path += 'M' + (x + quiet) + ' ' + (y + quiet) + 'h1v1h-1z';
      }
    }
    var el = document.createElementNS(SVG_NS, 'svg');
    el.setAttribute('viewBox', '0 0 ' + size + ' ' + size);
    el.setAttribute('shape-rendering', 'crispEdges');
    el.setAttribute('role', 'img');
    el.setAttribute('aria-label', label || 'QR code');
    el.setAttribute('class', 'qr');
    var bg = document.createElementNS(SVG_NS, 'rect');
    bg.setAttribute('width', size);
    bg.setAttribute('height', size);
    bg.setAttribute('fill', '#fff');
    el.appendChild(bg);
    var dark = document.createElementNS(SVG_NS, 'path');
    dark.setAttribute('d', path);
    dark.setAttribute('fill', '#000');
    el.appendChild(dark);
    return el;
  }

  global.OctoQr = { matrix: matrix, svg: svg };
  if (typeof module !== 'undefined' && module.exports) module.exports = global.OctoQr;
})(typeof window !== 'undefined' ? window : globalThis);
