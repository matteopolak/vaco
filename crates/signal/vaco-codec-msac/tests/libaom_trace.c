/* Verification-only harness; link against libaom 3.14.1's static library.
 * ABI: aom_dsp/entdec.h at tag v3.14.1 (BSD-2-Clause).
 * No reference implementation code is incorporated into this harness.
 * cc libaom_trace.c /opt/homebrew/lib/libaom.a -o /tmp/<private>/libaom_trace
 * libaom_trace <output-path>
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct od_ec_dec od_ec_dec;
extern void od_ec_dec_init(od_ec_dec *, const unsigned char *, uint32_t);
extern int od_ec_decode_cdf_q15(od_ec_dec *, const uint16_t *, int);
extern const char *aom_codec_version_str(void);

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    FILE *out = fopen(argv[1], "wb");
    if (!out) return 3;
    /* The tagged header's decoder state is 40 bytes on this machine.
     * Allocation keeps its representation private to the reference library. */
    od_ec_dec *decoder = calloc(1, 256);
    if (!decoder) return 4;
    uint8_t data[512];
    for (unsigned i = 0; i < sizeof(data); ++i)
        data[i] = (uint8_t)(i * 73 + (i >> 2) * 19 + 0xb4);
    const unsigned alphabets[] = {2, 3, 4, 7, 16};
    for (unsigned a = 0; a < sizeof(alphabets) / sizeof(*alphabets); ++a) {
        unsigned n = alphabets[a];
        uint16_t inverse_cdf[16];
        for (unsigned i = 0; i < n; ++i)
            inverse_cdf[i] = (uint16_t)(32768 - ((i + 1) * (i + 1) * 32768 / (n * n)));
        od_ec_dec_init(decoder, data, sizeof(data));
        for (unsigned i = 0; i < 128; ++i) {
            int symbol = od_ec_decode_cdf_q15(decoder, inverse_cdf, (int)n);
            if (symbol < 0 || symbol >= (int)n || fputc(symbol, out) == EOF) return 5;
        }
    }
    free(decoder);
    fprintf(stderr, "libaom %s: 640 symbols written\n", aom_codec_version_str());
    return fclose(out) == 0 ? 0 : 6;
}
