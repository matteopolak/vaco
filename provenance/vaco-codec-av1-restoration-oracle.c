/* Independent scalar dav1d oracle adapter; no filter equations are implemented here.
 * Build instructions and pinned source revision: docs/av1-loop-restoration.md.
 */
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "src/looprestoration_tmpl.c"

static int clamp(int v, int lo, int hi) { return v < lo ? lo : v > hi ? hi : v; }

static pixel sample(int x, int y, int depth, int after) {
    return ((x * 71 + y * 47 + x * y * 13 + after * 39) ^ ((x + y) * 29)) & ((1 << depth) - 1);
}

static void write_u16(unsigned v) {
    putchar(v & 255);
    putchar((v >> 8) & 255);
}

static void run(int depth, int width, int height, int sub_y, int unit_size, int mode) {
    const int stride = (width + 64) & ~31;
    const int cols = (width + unit_size / 2) / unit_size;
    const int rows = (height + unit_size / 2) / unit_size;
    const int unit_cols = cols > 0 ? cols : 1;
    const int unit_rows = rows > 0 ? rows : 1;
    pixel *after = calloc(stride * (height + 8), sizeof(pixel));
    pixel *work = calloc(stride * (height + 8), sizeof(pixel));
    pixel *output = calloc(stride * (height + 8), sizeof(pixel));
    pixel *lpf = calloc(stride * 8, sizeof(pixel));
    pixel (*left)[4] = calloc(64, sizeof(*left));
    assert(after && work && output && lpf && left);
    for (int y = 0; y < height; y++)
        for (int x = 0; x < width; x++)
            after[y * stride + x] = sample(x, y, depth, 1);
    Dav1dLoopRestorationDSPContext dsp;
    bitfn(dav1d_loop_restoration_dsp_init)(&dsp, depth);
    for (int stripe = 0; ; stripe++) {
        const int start = (stripe * 64 - 8) >> sub_y;
        const int end = start + (64 >> sub_y);
        const int y0 = start < 0 ? 0 : start;
        const int y1 = end > height ? height : end;
        if (y0 >= height) break;
        const int ur = clamp((y0 + (8 >> sub_y)) / unit_size, 0, unit_rows - 1);
        for (int uc = 0; uc < unit_cols; uc++) {
            const int x0 = uc * unit_size;
            const int x1 = uc + 1 == unit_cols ? width : x0 + unit_size;
            memcpy(work, after, stride * (height + 8) * sizeof(pixel));
            for (int i = 0; i < 8; i++) {
                const int row = clamp(i < 2 ? start - 2 + i : end + i - 6, 0, height - 1);
                for (int x = 0; x < width; x++) lpf[i * stride + x] = sample(x, row, depth, 0);
            }
            for (int y = y0; y < y1; y++)
                for (int x = 0; x < 4; x++)
                    left[y - y0][x] = after[y * stride + clamp(x0 - 4 + x, 0, width - 1)];
            enum LrEdgeFlags edges = (x0 > 0 ? LR_HAVE_LEFT : 0) |
                (x1 < width ? LR_HAVE_RIGHT : 0) | (y0 > 0 ? LR_HAVE_TOP : 0) |
                (y1 < height ? LR_HAVE_BOTTOM : 0);
            LooprestorationParams params = {0};
            looprestorationfilter_fn filter;
            const int unit_index = ur * unit_cols + uc;
            if (mode < 16) {
                const int set = (mode + unit_index) % 16;
                params.sgr.s0 = dav1d_sgr_params[set][0];
                params.sgr.s1 = dav1d_sgr_params[set][1];
                const int w0 = -96 + (mode * 11 + unit_index * 7) % 128;
                const int w1 = -32 + (mode * 17 + unit_index * 13) % 128;
                params.sgr.w0 = w0;
                params.sgr.w1 = 128 - w0 - w1;
                filter = dsp.sgr[params.sgr.s0 == 0 ? 1 : params.sgr.s1 == 0 ? 0 : 2];
            } else {
                const int coeffs[4][3] = {{-5, -23, -17}, {10, 8, 46}, {3, -7, 15}, {0, 0, 0}};
                for (int pass = 0; pass < 2; pass++) {
                    const int *c = coeffs[(mode - 16 + unit_index + pass) % 4];
                    int center = 128;
                    for (int tap = 0; tap < 3; tap++) {
                        const int value = sub_y && tap == 0 ? 0 : c[tap];
                        params.filter[pass][tap] = params.filter[pass][6 - tap] = value;
                        center -= 2 * value;
                    }
#if BITDEPTH == 8
                    if (pass == 0) center -= 128;
#endif
                    params.filter[pass][3] = center;
                }
                filter = dsp.wiener[sub_y];
            }
            const int bitdepth_max = (1 << depth) - 1;
            filter(work + y0 * stride + x0, stride * sizeof(pixel), left, lpf + x0,
                x1 - x0, y1 - y0, &params, edges HIGHBD_TAIL_SUFFIX);
            for (int y = y0; y < y1; y++)
                memcpy(output + y * stride + x0, work + y * stride + x0, (x1 - x0) * sizeof(pixel));
        }
    }
    for (int y = 0; y < height; y++)
        for (int x = 0; x < width; x++) write_u16(output[y * stride + x]);
    free(after); free(work); free(output); free(lpf); free(left);
}

static void constant_probe(int depth) {
    Dav1dLoopRestorationDSPContext dsp;
    bitfn(dav1d_loop_restoration_dsp_init)(&dsp, depth);
    for (int set = 0; set < 16; set++) {
        pixel image[32 * 16], lpf[32 * 8] = {0}, left[64][4] = {{0}};
        for (unsigned i = 0; i < sizeof(image) / sizeof(image[0]); i++)
            image[i] = (1 << depth) - 1;
        LooprestorationParams params = {0};
        params.sgr.s0 = dav1d_sgr_params[set][0];
        params.sgr.s1 = dav1d_sgr_params[set][1];
        params.sgr.w0 = -32;
        params.sgr.w1 = 129;
        const int bitdepth_max = (1 << depth) - 1;
        const int filter = params.sgr.s0 == 0 ? 1 : params.sgr.s1 == 0 ? 0 : 2;
        dsp.sgr[filter](image, 32 * sizeof(pixel), left, lpf, 7, 9, &params, 0 HIGHBD_TAIL_SUFFIX);
        for (int y = 0; y < 9; y++)
            for (int x = 0; x < 7; x++) write_u16(image[y * 32 + x]);
    }
}

int main(int argc, char **argv) {
    (void)argv;
    const int shapes[][4] = {{7, 9, 0, 64}, {13, 65, 0, 64}, {97, 130, 0, 64}, {48, 81, 1, 32}, {135, 129, 0, 128}};
#if BITDEPTH == 8
    const int depths[] = {8};
#else
    const int depths[] = {10, 12};
#endif
    if (argc > 1) {
        for (unsigned d = 0; d < sizeof(depths) / sizeof(depths[0]); d++) constant_probe(depths[d]);
        return ferror(stdout) ? EXIT_FAILURE : EXIT_SUCCESS;
    }
    for (unsigned d = 0; d < sizeof(depths) / sizeof(depths[0]); d++)
        for (unsigned shape = 0; shape < sizeof(shapes) / sizeof(shapes[0]); shape++)
            for (int mode = 0; mode < 20; mode++)
                run(depths[d], shapes[shape][0], shapes[shape][1], shapes[shape][2], shapes[shape][3], mode);
    return ferror(stdout) ? EXIT_FAILURE : EXIT_SUCCESS;
}
