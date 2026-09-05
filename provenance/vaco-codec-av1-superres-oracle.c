/* Independent scalar dav1d oracle adapter.
 *
 * The generator prepends the pinned `dav1d_resize_filter` table and the
 * unmodified `resize_c` body from dav1d.  This file supplies only inputs,
 * dimensions, and a stable little-endian output format; it contains no AV1
 * resampling arithmetic.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static void put_u16(unsigned value) {
    putchar(value & 255);
    putchar((value >> 8) & 255);
}

static uint32_t next_state(uint32_t state) {
    state ^= state << 13;
    state ^= state >> 17;
    return state ^ (state << 5);
}

static int scale_step(int input_width, int output_width) {
    return ((input_width << 14) + output_width / 2) / output_width;
}

static int scale_start(int input_width, int output_width, int step) {
    const int error = output_width * step - (input_width << 14);
    int start =
        (-((output_width - input_width) << 13) + output_width / 2) / output_width +
        (1 << 7) - error / 2;
    return start & ((1 << 14) - 1);
}

static void run_case(
    int bit_depth,
    int visible_input_width,
    int padded_input_width,
    int output_width,
    int height,
    unsigned case_id
) {
    const int stride = padded_input_width;
    pixel *input = calloc((size_t)stride * height, sizeof(*input));
    pixel *output = calloc((size_t)output_width * height, sizeof(*output));
    if (input == NULL || output == NULL) {
        free(input);
        free(output);
        exit(EXIT_FAILURE);
    }
    const unsigned max = (1u << bit_depth) - 1;
    uint32_t state = 0x6d2b79f5u ^ case_id;
    for (int y = 0; y < height; y++) {
        for (int x = 0; x < padded_input_width; x++) {
            state = next_state(state);
            input[y * stride + x] = (pixel)((state + x * 37u + y * 91u) & max);
        }
    }
    /* §7.16 derives the sampling phase from the visible FrameWidth, but
     * clamps taps to the MiCols-derived padded reconstruction width. */
    const int step = scale_step(visible_input_width, output_width);
    const int start = scale_start(visible_input_width, output_width, step);
    resize_c(
        output,
        output_width * (int)sizeof(*output),
        input,
        stride * (int)sizeof(*input),
        output_width,
        height,
        padded_input_width,
        step,
        start,
        max
    );
    put_u16((unsigned)bit_depth);
    put_u16((unsigned)visible_input_width);
    put_u16((unsigned)padded_input_width);
    put_u16((unsigned)output_width);
    put_u16((unsigned)height);
    for (int y = 0; y < height; y++) {
        for (int x = 0; x < output_width; x++) {
            put_u16(output[y * output_width + x]);
        }
    }
    free(input);
    free(output);
}

int main(void) {
    /* `(visible, padded, output, rows)`: include right Mi padding and odd
     * luma/chroma widths so the caller cannot accidentally clamp at visible
     * width instead of the §7.16 MiCols-derived bound. */
    const int shapes[][4] = {
        {7, 8, 8, 5}, {13, 16, 17, 7}, {31, 32, 40, 9},
        {47, 48, 64, 11}, {63, 64, 80, 13}, {95, 96, 120, 15},
    };
    unsigned case_id = 0;
    for (int bit_depth = 8; bit_depth <= 12; bit_depth += 2) {
        for (unsigned i = 0; i < sizeof(shapes) / sizeof(shapes[0]); i++, case_id++) {
            run_case(
                bit_depth,
                shapes[i][0],
                shapes[i][1],
                shapes[i][2],
                shapes[i][3],
                case_id
            );
        }
    }
    return ferror(stdout) ? EXIT_FAILURE : EXIT_SUCCESS;
}
