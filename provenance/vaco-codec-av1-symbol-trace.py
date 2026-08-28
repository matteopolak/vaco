#!/usr/bin/env python3
"""Independent Python transliteration of AV1 spec 8.2 (symbol decoder),
written directly from the specification text, not from the Rust source, to
serve as an independent trace for a unit test.
"""

EC_PROB_SHIFT = 6
EC_MIN_PROB = 4


def floor_log2(x):
    return x.bit_length() - 1


class BitReader:
    def __init__(self, data):
        self.data = data
        self.pos = 0  # bit position

    def get(self, n):
        # MSB-first, zero-padded past the end (matches vaco_bitstream::BitReader).
        v = 0
        for _ in range(n):
            byte_idx = self.pos // 8
            bit_idx = 7 - (self.pos % 8)
            bit = (self.data[byte_idx] >> bit_idx) & 1 if byte_idx < len(self.data) else 0
            v = (v << 1) | bit
            self.pos += 1
        return v


class SymbolDecoder:
    def __init__(self, data, disable_cdf_update=False):
        self.r = BitReader(data)
        sz = len(data)
        num_bits = min(sz * 8, 15)
        buf = self.r.get(num_bits)
        padded_buf = buf << (15 - num_bits)
        self.value = ((1 << 15) - 1) ^ padded_buf
        self.range = 1 << 15
        self.max_bits = 8 * sz - 15
        self.disable_cdf_update = disable_cdf_update

    def read_symbol(self, cdf):
        n = len(cdf) - 1
        cur = self.range
        symbol = -1
        while True:
            symbol += 1
            prev = cur
            f = (1 << 15) - cdf[symbol]
            cur = ((self.range >> 8) * (f >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT)
            cur += EC_MIN_PROB * (n - symbol - 1)
            if not (self.value < cur):
                break
        self.range = prev - cur
        self.value -= cur

        bits = 15 - floor_log2(self.range)
        self.range <<= bits
        num_bits = min(bits, max(0, self.max_bits))
        new_data = self.r.get(num_bits)
        padded_data = new_data << (bits - num_bits)
        self.value = padded_data ^ (((self.value + 1) << bits) - 1)
        self.max_bits -= bits

        if not self.disable_cdf_update:
            rate = 3 + (cdf[n] > 15) + (cdf[n] > 31) + min(floor_log2(n), 2)
            tmp = 0
            for i in range(n - 1):
                if i == symbol:
                    tmp = 1 << 15
                if tmp < cdf[i]:
                    cdf[i] -= (cdf[i] - tmp) >> rate
                else:
                    cdf[i] += (tmp - cdf[i]) >> rate
            cdf[n] += 1 if cdf[n] < 32 else 0
        return symbol


if __name__ == "__main__":
    data = bytes([0xB4, 0x2F, 0x91, 0x0C])
    sd = SymbolDecoder(data, disable_cdf_update=False)
    cdf = [1 << 14, 1 << 15, 0]
    out = [sd.read_symbol(cdf) for _ in range(6)]
    print(out)
