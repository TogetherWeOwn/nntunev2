// Generated structural fixture, never a media download. The production walker
// validates headers, continuity and CRCs, not decoded PCM or STREAMINFO MD5.
function crc(bytes, width, polynomial) {
  let value = 0;
  const high = 1 << (width - 1);
  const mask = (1 << width) - 1;
  for (const byte of bytes) {
    value ^= byte << (width - 8);
    for (let bit = 0; bit < 8; bit++) {
      value = ((value << 1) ^ (value & high ? polynomial : 0)) & mask;
    }
  }
  return value;
}

export function flacFixture() {
  const frameCount = 127;
  const samples = 256;
  const info = Buffer.alloc(34);
  info.writeUInt16BE(samples, 0);
  info.writeUInt16BE(samples, 2);
  info.writeBigUInt64BE((44100n << 44n) | (1n << 41n) | (7n << 36n)
    | BigInt(frameCount * samples), 10);
  const frames = [];
  for (let number = 0; number < frameCount; number++) {
    // Fixed-block stereo 8-bit verbatim subframes; frame numbers fit one byte.
    const header = Buffer.from([255, 248, 105, 18, number, samples - 1]);
    const body = Buffer.alloc(2 * (samples + 1));
    for (let channel = 0; channel < 2; channel++) {
      body[channel * (samples + 1)] = 2;
      for (let i = 0; i < samples; i++) {
        body[channel * (samples + 1) + i + 1] = (number + channel + i) & 127;
      }
    }
    const frame = Buffer.concat([header, Buffer.from([crc(header, 8, 7)]), body]);
    const footer = Buffer.alloc(2);
    footer.writeUInt16BE(crc(frame, 16, 0x8005));
    frames.push(frame, footer);
  }
  const audio = Buffer.concat(frames);
  // Padding must disappear when the real tagger rebuilds metadata.
  const input = Buffer.concat([Buffer.from('fLaC'), Buffer.from([0, 0, 0, 34]), info,
    Buffer.from([129, 0, 16, 0]), Buffer.alloc(4096), audio]);
  return { input, audio, samples: frameCount * samples };
}

export function metadataBlocks(bytes) {
  const blocks = [];
  let offset = 4;
  for (;;) {
    const last = (bytes[offset] & 128) !== 0;
    const type = bytes[offset] & 127;
    const length = bytes.readUIntBE(offset + 1, 3);
    blocks.push({ type, body: bytes.subarray(offset + 4, offset + 4 + length) });
    offset += 4 + length;
    if (last) return { blocks, audio: bytes.subarray(offset) };
  }
}
