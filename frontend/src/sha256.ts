const ROUND_CONSTANTS = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

const INITIAL_STATE = new Uint32Array([
  0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
  0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
]);

function rotateRight(value: number, amount: number) {
  return (value >>> amount) | (value << (32 - amount));
}

export class Sha256 {
  private readonly state = new Uint32Array(INITIAL_STATE);
  private readonly buffer = new Uint8Array(64);
  private bufferLength = 0;
  private bytesHashed = 0;
  private finished = false;

  update(data: Uint8Array) {
    if (this.finished) throw new Error('SHA-256 已完成');
    this.bytesHashed += data.byteLength;
    let offset = 0;
    if (this.bufferLength) {
      const copied = Math.min(64 - this.bufferLength, data.byteLength);
      this.buffer.set(data.subarray(0, copied), this.bufferLength);
      this.bufferLength += copied;
      offset += copied;
      if (this.bufferLength === 64) {
        this.processBlock(this.buffer);
        this.bufferLength = 0;
      }
    }
    while (offset + 64 <= data.byteLength) {
      this.processBlock(data.subarray(offset, offset + 64));
      offset += 64;
    }
    if (offset < data.byteLength) {
      this.buffer.set(data.subarray(offset), 0);
      this.bufferLength = data.byteLength - offset;
    }
  }

  digestHex() {
    if (!this.finished) {
      const bitLength = this.bytesHashed * 8;
      this.buffer[this.bufferLength] = 0x80;
      this.bufferLength += 1;
      if (this.bufferLength > 56) {
        this.buffer.fill(0, this.bufferLength);
        this.processBlock(this.buffer);
        this.bufferLength = 0;
      }
      this.buffer.fill(0, this.bufferLength, 56);
      const view = new DataView(this.buffer.buffer);
      view.setUint32(56, Math.floor(bitLength / 0x100000000));
      view.setUint32(60, bitLength >>> 0);
      this.processBlock(this.buffer);
      this.finished = true;
    }
    return Array.from(this.state, (value) => value.toString(16).padStart(8, '0')).join('');
  }

  private processBlock(block: Uint8Array) {
    const words = new Uint32Array(64);
    const view = new DataView(block.buffer, block.byteOffset, block.byteLength);
    for (let index = 0; index < 16; index += 1) words[index] = view.getUint32(index * 4);
    for (let index = 16; index < 64; index += 1) {
      const previous = words[index - 15];
      const prior = words[index - 2];
      const smallSigma0 = rotateRight(previous, 7) ^ rotateRight(previous, 18) ^ (previous >>> 3);
      const smallSigma1 = rotateRight(prior, 17) ^ rotateRight(prior, 19) ^ (prior >>> 10);
      words[index] = (words[index - 16] + smallSigma0 + words[index - 7] + smallSigma1) >>> 0;
    }
    let [a, b, c, d, e, f, g, h] = this.state;
    for (let index = 0; index < 64; index += 1) {
      const bigSigma1 = rotateRight(e, 6) ^ rotateRight(e, 11) ^ rotateRight(e, 25);
      const choose = (e & f) ^ (~e & g);
      const temporary1 = (h + bigSigma1 + choose + ROUND_CONSTANTS[index] + words[index]) >>> 0;
      const bigSigma0 = rotateRight(a, 2) ^ rotateRight(a, 13) ^ rotateRight(a, 22);
      const majority = (a & b) ^ (a & c) ^ (b & c);
      const temporary2 = (bigSigma0 + majority) >>> 0;
      h = g;
      g = f;
      f = e;
      e = (d + temporary1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (temporary1 + temporary2) >>> 0;
    }
    this.state[0] = (this.state[0] + a) >>> 0;
    this.state[1] = (this.state[1] + b) >>> 0;
    this.state[2] = (this.state[2] + c) >>> 0;
    this.state[3] = (this.state[3] + d) >>> 0;
    this.state[4] = (this.state[4] + e) >>> 0;
    this.state[5] = (this.state[5] + f) >>> 0;
    this.state[6] = (this.state[6] + g) >>> 0;
    this.state[7] = (this.state[7] + h) >>> 0;
  }
}

export async function digestFile(file: File): Promise<string> {
  const hasher = new Sha256();
  if (typeof file.stream === 'function') {
    const reader = file.stream().getReader();
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      hasher.update(chunk.value);
    }
  } else {
    const chunkSize = 1024 * 1024;
    for (let offset = 0; offset < file.size; offset += chunkSize) {
      hasher.update(new Uint8Array(await file.slice(offset, offset + chunkSize).arrayBuffer()));
    }
  }
  return hasher.digestHex();
}
